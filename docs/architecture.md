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

Traditional mode also keeps the daemon small — about 3.5k lines of Rust
including comments, plus a diagnostic probe binary — for configuration,
watching the kernel's state, and driving cull.

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
cull=N frun=H fcull=H fstop=H brun=H bcull=H bstop=H freleased=H breleased=H
```

`cull=1` means the kernel wants culling: it is set when free space drops
below `bcull`/`fcull` and cleared once free space is back at
`brun`/`frun`. The kernel re-checks free space on every read and on cache
writes. The other fields are **the limits themselves, not free counts**:
the configured percentages converted at bind time into filesystem blocks
(`b*`) and inodes (`f*`), in hex (no `0x` prefix). They stay fixed while
the cache is bound; the kernel compares them with `f_bavail` / `f_ffree`
from `statfs(cache_dir)`, minus the blocks of cache writes still in
flight. `freleased` / `breleased` count the objects (and blocks) the
kernel stopped using since the previous read; the daemon ignores them.
The daemon's heartbeat log prints the current `bavail` / `ffree` next to
the limits.

The kernel sets `POLLIN` when the state line changed and `POLLOUT` for as
long as culling remains active. The daemon polls for `POLLIN` only:
`POLLOUT` would spin the loop whenever it decides to wait.

The daemon also reads the state whenever `poll` times out (at least every
5 s, sooner when a timer or a cull decision is due). The kernel re-checks
free space only when the cache writes or the daemon reads, so without
these reads an idle cache would never be told to cull while other data
fills a shared filesystem.

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

The percentages apply to the whole filesystem under `cache_dir`, not just
to cached data. When `cache_dir` is a subdirectory of a volume that also
holds other data, that data counts too: the cache grows until the
*filesystem* is 93 % full and gives space back as the other data grows.
If the other data alone keeps free space below `brun`, culling cannot
reach its target and ends up emptying the cache.

### `max_size`: a budget through a project quota

The kernel's percentages are of whatever `statfs(cache_dir)` reports, and
on XFS and ext4 statfs on a directory that carries a project ID with the
inherit flag reports the project's block limit as the size, and the
smaller of "limit minus usage" and the real free space as available.
With `max_size` set, the daemon (`quota.rs`), before bind:

1. checks that project quota accounting and enforcement are on
   (`quotactl_fd`, `Q_XGETQSTATV`);
2. refuses a `quota_project_id` that already holds data elsewhere, or a
   cache directory already tagged with another project;
3. gives every file and directory of the cache tree the project ID, and
   directories the inherit flag, so what the kernel creates later carries
   it too (`FS_IOC_FSSETXATTR`);
4. sets the project's block hard limit to `max_size` (soft limit 0, so
   statfs reports the hard one), and checks that statfs on `cache_dir` now
   reports it.

The kernel computes its limits from that statfs at bind, so the
percentages become percentages of the budget, while a filesystem that
runs low still makes the cache shrink. Two details matter:

- statfs goes to the root of the mount the kernel was given, which is why
  `cache_dir` must be its own mount (the self-bind) for the quota to show.
- The whole tree is tagged before bind. XFS refuses renames between
  directories of different projects (`EXDEV`), and cachefiles treats a
  failed rename into its graveyard as an I/O error that stops the cache.

## Cull algorithm

The on-disk layout cachefiles produces (kernel ≥ 5.17) is:

```text
<cache_root>/cache/Ivolume,…/             ← volume index   (depth 1)
<cache_root>/cache/Ivolume,…/@xx/         ← hash bucket    (depth 2)
<cache_root>/cache/Ivolume,…/@xx/Scookie  ← cookie object  (depth 3)
```

While the kernel reports `cull=1`, the daemon asks the planner
(`planner.rs`, pure logic) what to do on every wakeup:

1. **Size the pass** from `statfs(cache_dir)`: the blocks and inodes
   missing to bring free space up to `brun` / `frun`, plus a small margin
   above `brun` (a tenth of the `bcull`..`brun` band, at most 1 GiB): the
   kernel only clears `cull=1` once free space *minus in-flight cache
   writes* reaches `brun`, so stopping exactly at `brun` leaves the flag set
   under steady writes. Space earlier passes freed in the background but
   statfs does not show yet is subtracted (see step 5). If nothing is
   missing, look again in a second.
2. **Empty the graveyard** (`<cache_dir>/graveyard`, where the kernel moves
   directories it deletes), counting what it held toward the target.
3. **Take candidates** from the table the previous walk left, or walk the
   cache: only depth 3 under `<cache_dir>/cache/` (`min_depth(3)` /
   `max_depth(3)`):
   - skipping depth 1 protects the volume index — `cull Ivolume,…`
     succeeds whenever no client mount is currently using the volume
     (boot before mount, after umount, e2e setup) and erases the
     entire cache;
   - skipping depth 2 avoids the hash buckets, which the kernel would
     reject anyway;
   - capping at depth 3 prevents the daemon from descending into an
     index-cookie directory and trying to cull its children
     individually — culling the parent removes the subtree.

   A walk keeps the K oldest-by-atime objects as `(bucket, name, atime,
   allocated bytes)`, leaving out objects set aside (step 4). K starts at
   `cull.batch_size` and grows, up to 64 times that, to fit what is left to
   free given the average object size of the last walk. Allocated bytes
   are `st_blocks × 512`, not `st_size`: cachefiles sizes each cache file
   to the whole remote file but only stores the ranges that were read, so
   `st_size` can be several times what culling the file gives back. What a
   pass does not use stays in the table for the next pass, for up to
   5 minutes.
4. **Cull oldest first and stop as soon as the target is met.** For each
   candidate: re-check its atime (a changed one was just used: skip it),
   open it, `chdir(bucket)` (skipped when the bucket is the previous one's),
   send `cull <name>`, then free the blocks:
   - `EBUSY`: the kernel uses the object. It is set aside — left out of
     walks for 5 minutes — instead of coming back on every walk. Without
     that, a batch of busy objects (files tenants keep open or mapped),
     being the oldest, came back unchanged from every walk and the cull
     never reached anything it could free.
   - `ENOENT` / `ESTALE`: the object went away meanwhile; not an error.
   - `EIO`: the kernel has most likely stopped the cache; the pass ends
     and the daemon checks (see "Failure modes").
   - Success: the kernel has unlinked the file. Once `st_nlink` on the
     descriptor we opened is 0, no one else can reach the inode (cachefiles
     never culls an object it uses and never picks a culled one up again),
     and the daemon truncates it through that descriptor. XFS would
     otherwise free the blocks in background workers, seconds later for a
     file of hundreds of thousands of extents (measured: 4–5 s for 200k),
     and statfs — and the kernel — would count them as used meanwhile. The
     truncate frees them before the pass ends.

   When the batch runs out first, walk again. A batch that frees nothing
   and sets nothing new aside ends the pass.
5. **Record the pass.** Statfs right after it is accurate, so the next
   decision is sized on fresh numbers. What could not be freed on the spot
   (directories, a failed truncate) the planner remembers as pending and
   subtracts from later targets until statfs gains it, or for 60 s at most
   so that space other writes take meanwhile is still given back.
6. **Back off** when a pass freed nothing (everything left is busy,
   failing, or the cache is empty while other data keeps free space low):
   wait 1 s, doubling up to 60 s, and log the warning at most every five
   minutes.

Version 1.0.2 culled a whole batch per pass and re-read the state right
away, which on caches made of a few thousand large files emptied nearly
the whole cache every time free space crossed `bcull`. 1.0.3 sized passes
but waited at most 30 s for statfs to catch up, after which a second pass
could cull the same shortfall again; truncating culled files on the spot
removed the wait.

This is the same shape as upstream cachefilesd's algorithm (including its
table of candidates kept between passes). The walk-on-demand approach
scales to caches with hundreds of thousands of objects without keeping a
persistent index in memory.

## Failure modes

| situation | outcome |
|-----------|---------|
| /dev/cachefiles missing | daemon fails fast at startup |
| another daemon holds the device | open returns EBUSY |
| cache dir not its own mountpoint | `bind` returns EINVAL; daemon exits |
| limits violate ordering | rejected before any kernel write |
| daemon crash / SIGKILL | kernel auto-withdraws cache on fd close; systemd restarts the daemon |
| cache fs fills past `bstop` | kernel refuses new caching; warns in dmesg |
| cull command races against kernel use | EBUSY; object set aside for 5 min, daemon tries the next |
| nothing cullable while `cull=1` | pass makes no progress; backoff 1 s → 60 s, rate-limited warning |
| kernel stops the cache after an I/O error (state `E`; e.g. a cache xattr write failing with ENOSPC on a full disk) | every command fails with EIO and nothing is cached; the daemon notices within 10 s (or on the first EIO) and exits non-zero, systemd binds it again |
| `max_size` set but project quotas not enforced | daemon refuses to start, says why |
| daemon hangs | systemd watchdog (2 min) restarts it |
| malformed state line from kernel | logged as warn (rate-limited); loop continues |

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
  main.rs            CLI, signal handlers, tracing init, runs Daemon
  config.rs          TOML schema, validation, ConfigCmd conversion
  daemon.rs          poll loop, state read, health check, timers, heartbeat
  planner.rs         when to cull and how much (pure logic)
  cull.rs            directory walk, candidate table, chdir + cull, truncate
  quota.rs           max_size as a project quota (quotactl_fd)
  fsattr.rs          FS_IOC_FS{GET,SET}XATTR: project IDs, extent size hints
  metrics.rs         node_exporter textfile output
  systemd_notify.rs  sd_notify: readiness, watchdog, start timeout
  signals.rs         SIGTERM/SIGINT/SIGHUP wiring
  util.rs            log rate limiting, byte formatting
  error.rs           thiserror enum
  proto/
    mod.rs           module re-exports + /dev/cachefiles path constant
    cmd.rs           Kernel trait, Device, ConfigCmd::apply_and_bind, cull(), inuse()
    state.rs         parser for the kernel's state line
  bin/probe.rs       diagnostic binary

packaging/
  systemd/nfs-cachefs.service
  etc/nfs-cachefs/daemon.toml     default config
  share/man/man8/nfs-cachefs.8    manpage
  install.sh                      installer

tests/
  e2e/nfs-fscache.sh              end-to-end with a real NFS server
```
