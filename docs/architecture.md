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
 │ cache filesystem (NVMe)  │  ext4/xfs on a dedicated mountpoint
 └──────────────────────────┘                     ▲
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

Traditional mode also yields the simplest daemon: ~700 LOC of Rust to do
configuration, monitor `/proc/fs/fscache/stats`-equivalent state, and
drive cull.

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
current free counts in hex (no `0x` prefix). The kernel sets the device
readable when it wants the daemon to re-evaluate.

## Cull algorithm

When `cull=1`:

1. Walk the cache directory (`<cache_dir>/cache/...`) using `walkdir`.
2. For each regular file collect `(parent, basename, atime, size)`.
3. Sort by `atime` ascending (oldest first).
4. Process at most `cull.batch_size` candidates. For each:
   - `chdir(parent)`
   - Send `cull <basename>`. Kernel returns `EBUSY` if held → skip.
5. Re-poll state. If `cull=0`, done; else repeat.

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

## Performance expectations

Validated in a QEMU VM with loop-ext4 backing (the user's NFSv3 setup):

| read | result | bottleneck |
|------|--------|-----------|
| 1 GB cold (fresh page cache) | ~850 MB/s | network bandwidth |
| 1 GB hot (after drop_caches) | ~750–950 MB/s | loop ext4 / VM disk |
| fscache IO `rd` counter | +1025 per 1 GB hot | confirms cache hit path |
| cache dir size after 1 GB read | 1.1 GB sparse file (logical 4.97 GB = source size) | sparse population |

On real PCIe 5.0 NVMe + DDR5 expect single-digit GB/s on hot reads. Cold
reads are bounded by NFS network bandwidth regardless.

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
