# Architecture

## Design constraints

- **Transparent to applications**: NFS-mounted paths must remain unchanged.
  No FUSE, no LD_PRELOAD, no application changes.
- **Approach native NVMe bandwidth on cache hits**: avoids the FUSE-style
  triple-memcpy cost that would otherwise cap throughput at ~25–40% of
  PCIe 5.0 NVMe.
- **Modern kernel only**: minimum Ubuntu 24.04 LTS / kernel 6.8. The
  legacy `cachefilesd` does not run cleanly on this release.

## Stack

```
 ┌─────────────────┐
 │  Application    │  reads file via libc read()
 └────────┬────────┘
          │
 ┌────────▼────────┐
 │ Linux VFS       │
 └────────┬────────┘
          │
 ┌────────▼─────────────────┐
 │ NFS client (-o fsc)      │  decides what to cache, when to invalidate
 │  fs/nfs/fscache.c        │  (close-to-open consistency, change attr)
 └────────┬─────────────────┘
          │  netfs API
 ┌────────▼─────────────────┐
 │ fscache framework        │  generic cache abstraction
 │  fs/netfs + fs/fscache   │
 └────────┬─────────────────┘
          │
 ┌────────▼─────────────────┐
 │ cachefiles backend       │  stores objects as files in a host fs
 │  fs/cachefiles/          │  emits state on /dev/cachefiles
 └─────────┬───────▲────────┘
           │       │ /dev/cachefiles
 ┌─────────▼───────┴────────┐
 │ cache filesystem (NVMe)  │  ext4/xfs on its own mountpoint —
 └──────────────────────────┘  bind-mount, partition, or loop image
                                                  ▲
                                                  │ writes "dir/tag/limits/bind",
                                                  │ reads "cull=N frun=...",
                                                  │ writes "cull <name>"
                                ┌──────────────────┴─────────────┐
                                │ nfs-cachefs daemon (this repo) │
                                │ - boot: configure + bind       │
                                │ - run:  poll + read state line │
                                │ - cull: walkdir + atime + cull │
                                └────────────────────────────────┘
```

The daemon is **not on the data path**. Once `bind` succeeds, every read
flows kernel-side through cachefiles → NVMe. The daemon's only runtime job
is to delete LRU objects when free space drops.

## Why traditional mode (not on-demand)

The kernel exposes two cachefiles modes:

| mode | when it fires daemon events | typical user |
|------|------------------------------|--------------|
| traditional | only when cache space gets low | NFS, AFS, CIFS, Ceph |
| on-demand   | every cache-miss / open       | erofs / Nydus container images |

On-demand was added in kernel 5.19 specifically for container image lazy
loading. Stock Ubuntu 24.04 builds with `CONFIG_CACHEFILES_ONDEMAND=n`,
verified at `/boot/config-$(uname -r)`. Even a custom-built kernel with
on-demand enabled would not be a natural fit for NFS, because the NFS
client itself populates the cache via fscache writes — there's nothing for
the daemon to fetch.

Traditional mode also yields the simplest daemon: ~900 LOC of production
Rust (plus a ~300 LOC diagnostic probe binary) to do configuration,
monitor `/proc/fs/fscache/stats`-equivalent state, and drive cull.

## Protocol

The daemon talks to the kernel via a single fd on `/dev/cachefiles`.

### Daemon → kernel (write)

Single command per `write()`. No newline terminator. The kernel parses the
buffer as one command.

| command            | purpose |
|--------------------|---------|
| `dir <path>`       | cache directory (must be its own fs) |
| `tag <name>`       | unique identifier |
| `secctx <ctx>`     | optional SELinux/AppArmor context |
| `brun N%` / `bcull N%` / `bstop N%` | block free-space thresholds |
| `frun N%` / `fcull N%` / `fstop N%` | inode free thresholds |
| `bind`             | activate (must be last config) |
| `cull <name>`      | delete object `name` from CWD |
| `inuse <name>`     | check if kernel holds object `name` (CWD) |
| `freleased <name>` | (legacy, not used here) |

`cull` and `inuse` require the daemon's CWD to be the parent directory of
the named object — kernel resolves via the daemon task's `current->fs`.
There is **no** `unbind` command; closing the fd unbinds the cache.

### Kernel → daemon (read)

A single line is returned on each successful `read()`:

```
cull=N frun=H fcull=H fstop=H brun=H bcull=H bstop=H
```

`cull=1` means the daemon should start culling. The other fields are the
current free counts in hex (no `0x` prefix). The kernel sets `POLLIN`
when the state line changed and `POLLOUT` while culling remains active.
The daemon listens for both so one cull batch that does not reach `run`
does not stall further culling.

## Cache sizing

There is no fixed-bytes cache limit. The cache fills the backing
filesystem you mount at `cache_dir`; "size" is shaped by three percentage
thresholds the kernel evaluates against `statfs(cache_dir)`:

| key                  | role         | default |
|----------------------|--------------|--------:|
| `brun`  / `frun`     | cull target — daemon stops culling once free reaches this | 10 % |
| `bcull` / `fcull`    | low-water  — kernel sets `cull=1` below this              |  7 % |
| `bstop` / `fstop`    | hard floor — kernel refuses new caching below this        |  3 % |

`b*` apply to blocks (capacity); `f*` to inodes (file count). The daemon
validates `stop < cull < run < 100` both at config load
(`Config::validate`) and right before sending the limit commands
(`ConfigCmd::apply_and_bind`).

So a 100 G cache fs with defaults effectively oscillates between roughly
93 G and 90 G of cached data: kernel signals `cull=1` at 93 G occupied
(7 % free); daemon evicts oldest-by-atime until 90 G occupied (10 % free)
and stops. Want a smaller working set? Make the backing fs smaller, or
raise `bcull` / `brun` to leave more headroom. There is intentionally no
absolute-bytes knob — the kernel protocol is percentage-only.

## Cull algorithm

The on-disk layout cachefiles produces (kernel ≥ 5.17) is:

```text
<cache_root>/cache/Ivolume,…/             ← volume index   (depth 1)
<cache_root>/cache/Ivolume,…/@xx/         ← hash bucket    (depth 2)
<cache_root>/cache/Ivolume,…/@xx/Scookie  ← cookie object  (depth 3)
```

When `cull=1`:

1. Delete entries that the kernel moved into `<cache_dir>/graveyard`.
2. Walk **only depth 3** under `<cache_dir>/cache/` using `walkdir`
   (`min_depth(3).max_depth(3)`). Both bounds matter:
   - skipping depth 1 protects the volume index — `cull Ivolume,…`
     succeeds whenever no client mount is currently using the volume
     (boot before mount, after umount, e2e setup) and erases the
     entire cache;
   - skipping depth 2 avoids the hash buckets, which the kernel would
     reject anyway;
   - capping at depth 3 prevents the daemon from descending into an
     index-cookie directory and trying to cull its children
     individually — culling the parent removes the subtree.
3. For each cookie collect `(parent, basename, atime, size)`.
4. Sort by `atime` ascending (oldest first).
5. Process at most `cull.batch_size` candidates. For each:
   - `chdir(parent)`
   - Send `cull <basename>`. Kernel returns `EBUSY` if held → skip.
6. Re-poll state. If `cull=0`, done; else repeat. If a pass makes no
   progress because every candidate is busy or errored, briefly back off
   before trying again.

This is the same shape as upstream cachefilesd's algorithm. The
walk-on-demand approach scales to caches with hundreds of thousands of
objects without keeping a persistent index in memory.

## Failure modes

| situation | outcome |
|-----------|---------|
| /dev/cachefiles missing | daemon fails fast at startup |
| another daemon holds the device | open returns EBUSY |
| cache dir not its own mountpoint | `bind` returns EINVAL; daemon exits |
| limits violate ordering | rejected before any kernel write |
| daemon crash / SIGKILL | kernel auto-withdraws cache on fd close |
| cache fs fills past `bstop` | kernel refuses new caching; warns in dmesg |
| cull command races against kernel use | EBUSY; daemon skips and tries next |
| malformed state line from kernel | logged as warn; loop continues |

## Performance baseline

The interesting variables are the **cache backend kind**
(bind-mount / loop image / dedicated partition) and **read concurrency**.
Storage and NIC set the ceiling; fscache layer overhead and per-cookie
locking shape the curve.

### Single-thread reads (Ubuntu 24.04 / kernel 6.8, NFSv3, 1 MiB bs after drop_caches)

Reference rig A — 2× Intel D7-P5510 in md RAID0, 50 Gbps NIC,
**bind-mount** cache_dir on host xfs:

| path                                | throughput   |
|-------------------------------------|-------------:|
| md RAID0 direct, qd=1               | 2.7 GB/s     |
| xfs file direct, qd=1               | 4.5 GB/s     |
| **NFS cold (network → cache)**      | 0.7–1.2 GB/s |
| **NFS hot (fscache hit, pagecache)**| **1.4–1.8 GB/s** |

Reference rig B — single WD Ultrastar SN640 PCIe Gen3 ×4 NVMe, 5 Gbps NIC,
**loop-xfs** cache image (the original validation rig):

| path                                | throughput   |
|-------------------------------------|-------------:|
| raw `/dev/nvme0n1` direct           | 1.9 GB/s     |
| raw xfs file, direct                | 1.6 GB/s     |
| raw xfs file, page cache            | 730–970 MB/s |
| **NFS cold (network → cache)**      | 695 MB/s     |
| **NFS hot, steady-state**           | 1.8 GB/s     |
| NFS hot, **first read after cold**  | 250–600 MB/s |

The "first read after cold" dip is specific to loop image — the loop
driver's host-pagecache hasn't been populated yet, so the first hot
read pays a buffered-but-cold-host-fs cost on top of fscache. Bind-mount
hits the host xfs directly with no intermediate pagecache layer and
shows no such dip. On rig B's slow NIC the loop overhead is masked by
the smaller absolute throughput; on rig A it dominates.

### Multi-thread scaling (rig A, bind-mount)

Single-thread is bottlenecked by `netfs`/`cachefiles` per-cookie locking
and CPU memcpy, not disk. Concurrency gets you to the storage ceiling:

| concurrency             | aggregate    | per-worker | comment |
|-------------------------|-------------:|-----------:|---------|
| qd=1                    | 2.4 GB/s     | 2.4        | baseline |
| qd=4 same file          | 5.0 GB/s     | 1.25       | per-cookie lock plateau |
| qd=8 same file          | 4.6 GB/s     | 0.6        | plateaued |
| qd=4 distinct files     | 6.3 GB/s     | 1.6        | scales |
| **qd=8 distinct files** | **10.0 GB/s**| 1.25       | saturates md2 RAID0 |

fscache parallelizes well across file cookies but serializes within a
single cookie (a single NFS file). Per-file caps at ~5 GB/s on this
hardware regardless of how many readers; per-rig caps near the RAID0
direct-IO ceiling (9.86 GB/s).

LLM weight loaders that open N safetensors shards in parallel get
~10 GB/s on rig A directly. Single-threaded `dd` benchmarks understate
the real-workload throughput.

### Layer ceilings on rig A

| layer                    | qd=1     | qd=8     |
|--------------------------|---------:|---------:|
| md RAID0 direct          | 2.7 GB/s | 9.9 GB/s |
| xfs file direct          | 4.5 GB/s | 8.1 GB/s |
| **loop-xfs file direct** | 2.3 GB/s | 3.0 GB/s |

The loop driver caps ~3 GB/s regardless of underlying storage (it's
single-threaded inside the kernel, and double-buffers via host
pagecache). On a multi-NVMe rig that's the binding constraint —
hence the bind-mount recommendation.

The FUSE-based predecessor (wiped in commit `76f5744`) topped out at
~25–40 % of NVMe — the whole point of this rewrite is staying on the
kernel data path. Don't reintroduce a FUSE/userspace data path. On
PCIe 5.0 NVMe + DDR5 expect proportionally higher ceilings.

## Why no tokio / dashmap / parking_lot

- One open fd, low event rate (state changes are seconds-apart).
- Cull is single-threaded sequential walk; no concurrency hot path.
- `std::sync` and a single signal-handled `AtomicBool` cover all
  synchronization.
- Smaller binary (~1.9 MB stripped), faster compile, fewer dependencies.

## Layout summary

```
src/
  main.rs        CLI, signal handlers, tracing init, runs Daemon
  config.rs      TOML schema, validation, ConfigCmd conversion
  daemon.rs      poll loop, state read, cull trigger, heartbeat
  cull.rs        directory walk, atime sort, chdir + cull commands
  error.rs       thiserror enum
  proto/
    mod.rs       module re-exports + /dev/cachefiles path constant
    cmd.rs       Device wrapper, ConfigCmd::apply_and_bind, cull(), inuse()
    state.rs     parser for the kernel's state line
  bin/probe.rs   throwaway diagnostic binary

packaging/
  systemd/nfs-cachefs.service
  etc/nfs-cachefs/daemon.toml     default config
  share/man/man8/nfs-cachefs.8    manpage
  install.sh                      installer

tests/
  e2e/nfs-fscache.sh              end-to-end with a real NFS server
```
