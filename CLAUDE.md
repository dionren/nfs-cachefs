# nfs-cachefs — agent notes

Rust userspace daemon for the Linux kernel's **fscache + cachefiles** path.
Drop-in replacement for the stagnant upstream `cachefilesd`. Minimum
supported platform is **Ubuntu 24.04 LTS / kernel 6.8**.

Read first: `README.md`, `docs/architecture.md`.

## Test machine

There is a remote test box used to validate every change end-to-end
against a real NFS server. **Don't put host names, IPs, paths, or
credentials in this repo** — those live in agent memory (search for
"test machine" / "test_machine_setup" entries). The repo only encodes
the generic invariants below.

- **Build on the dev host, deploy stripped release binary.** Test box
  has no toolchain on purpose; never `cargo build` over SSH. Both ends
  are Ubuntu 24.04 / glibc 2.39 so the binary copies cleanly.
- **Module is in-tree but not autoloaded** on 24.04 — `modprobe
  cachefiles` before launching the daemon. (`cachefiles.ko` is the
  *kernel* module; distinct from the legacy userspace `cachefilesd`
  this project replaces.)
- **Cache dir convention**: a **self-bind-mounted subdirectory** on
  the host's NVMe xfs (e.g.,
  `mount --bind /mnt/nvme/cache /mnt/nvme/cache`). Cachefiles' "must
  be its own mountpoint" check is `mnt->mnt_root == dentry`, which a
  self-bind satisfies. This is preferred over a loop-mounted image
  because it skips the loop driver entirely (no host-pagecache
  double-buffering, no "first-hot-after-cold" warm-up penalty).
  Loop images and dedicated partitions also work; bind-mount is just
  the simplest and fastest on shared NVMe.
- **NFS test mount**: a separate `fsc,nosharecache` mount, never the
  test box's existing production NFS mount. If the same export is
  already mounted elsewhere on the box, the kernel coalesces NFS SBs
  by `(server, export)` unless `nosharecache` is given — without it,
  the new `-o fsc` mount silently inherits the existing non-fsc SB
  and `/proc/fs/nfsfs/volumes` shows FSC=no.
- **Daemon must be bound before any `mount -o fsc`.** Otherwise the
  mount silently falls back to no-caching with no error logged.

## Repo layout

```
src/main.rs          CLI, signals, tracing
src/config.rs        TOML schema + validation
src/daemon.rs        poll loop, state read, cull trigger, heartbeat
src/cull.rs          walk + atime sort + chdir + cull
src/error.rs         thiserror enum
src/proto/cmd.rs     Device, ConfigCmd::apply_and_bind, cull(), inuse()
src/proto/state.rs   parse the kernel state line
src/proto/mod.rs     re-exports + CACHEFILES_DEV constant
src/bin/probe.rs     diagnostic-only binary

packaging/{etc,systemd,share,install.sh}
tests/e2e/nfs-fscache.sh   needs root + real NFS server
```

Total ~1.2k LOC. No tokio / parking_lot / dashmap on purpose — single fd,
low event rate, single-threaded cull. Don't add async runtime without a
clear motivation.

## Build / run

```sh
cargo build --release       # → target/release/{nfs-cachefs,nfs-cachefs-probe}
sudo packaging/install.sh   # → /usr/sbin, /etc/nfs-cachefs, systemd, manpage
```

Toolchain: Rust ≥ 1.75 (stable). Release profile uses thin LTO, single
codegen unit, stripped symbols → ~1.9 MB stripped binary.

## Hard constraints (don't fight these)

- **Traditional mode only.** Stock Ubuntu 24.04 kernel ships with
  `CONFIG_CACHEFILES_ONDEMAND=n` (verify via
  `grep CACHEFILES /boot/config-$(uname -r)`). `bind ondemand` is
  rejected by the kernel. Daemon role is configurator + cull driver,
  **not** per-request mediator.
- **Cache dir must be its own mountpoint.** `bind` returns `EINVAL`
  otherwise. Three setups satisfy the check; xfs or ext4 (with
  `user_xattr`) underneath in any case:
  - **Self-bind a subdir** — `mount --bind /path /path`. Preferred:
    no loop overhead, no extra fs to manage. The kernel only checks
    `mnt->mnt_root == dentry`, which a self-bind makes true.
  - **Loop-mounted image** — works but adds a host-pagecache layer
    and a "first hot read after cold" warm-up cost.
  - **Dedicated partition / LV** — fine but invasive on shared disks.
- **Only one daemon can hold `/dev/cachefiles`.** Open returns `EBUSY` if
  another holds it. There is **no `unbind` command** — closing the fd
  unbinds (kernel 6.8 returns `ENOTSUPP` on `unbind`). `Device::Drop`
  handles this; do not write `unbind`.
- **CWD requirement for `cull` / `inuse`.** Kernel resolves the object
  name via `current->fs`, so the daemon must `chdir(parent)` first. See
  `cull::run_pass`. This is why cull is single-threaded.
- **Limits ordering**: `stop < cull < run < 100`, separately for `b*`
  (blocks) and `f*` (inodes). Validated in both `Config::validate` and
  `ConfigCmd::apply_and_bind`.
- **Daemon must start before any `mount -o fsc`.** Otherwise the mount
  silently falls back to no caching. Systemd unit orders
  `Before=remote-fs-pre.target`.
- **Needs `CAP_SYS_ADMIN`** to write `/dev/cachefiles`. Unit declares
  `CapabilityBoundingSet=CAP_SYS_ADMIN CAP_DAC_READ_SEARCH`.

## Protocol gotchas

- One command per `write()`. Don't batch. No newline terminator.
- `cull` and `inuse` returning `EBUSY` (kernel holds the object) is
  normal — caller treats as "skip, try later" via `Ok(false)`. See
  `proto/cmd.rs::cull`.
- Kernel state line is space-separated `k=v`; `cull` is decimal, others
  are **hex without `0x`**. Parser ignores unknown fields for forward
  compat (`proto/state.rs`).
- The kernel marks the device readable to wake the daemon up for state
  changes; we still poll with a 5 s timeout to drive heartbeat + check
  the stop flag.

## Verification commands (kernel side)

```
cat /proc/fs/nfsfs/volumes        # FSC column = yes for fsc-mounted volumes
cat /proc/fs/fscache/caches       # state column = A (active) after bind
cat /proc/fs/fscache/volumes      # one row per fsc-mounted NFS server
cat /proc/fs/fscache/stats        # Stores/RdHelp counters move on access
cat /proc/fs/fscache/cookies      # per-object cookies (NFS.server entries)
ls /var/cache/fscache/{cache,graveyard}   # cachefiles creates these on bind
journalctl -u nfs-cachefs -f      # daemon logs (or /var/log/nfs-cachefs.log if running raw)
```

## Performance baseline (already validated)

### Single-thread reads (1 MiB bs after drop_caches)

Numbers depend heavily on cache backend kind, NIC, and the underlying
storage; the cache backend kind is the lever the daemon controls.
Two reference points:

**Bind-mount cache on RAID0 NVMe + 50 Gbps NIC**:

| path                                 | throughput   |
|--------------------------------------|-------------:|
| NFS cold (network → cache)           | 0.7–1.2 GB/s |
| NFS hot (fscache hit, pagecache)     | **1.4–1.8 GB/s** |
| no first-hot warm-up; reads are stable from the first hot pass |

**Loop-image cache on single SN640 + 5 Gbps NIC** (the original
validation rig):

| path                                 | throughput   |
|--------------------------------------|-------------:|
| NFS cold (network → cache)           | 695 MB/s     |
| NFS hot (fscache hit, pagecache)     | 1.8 GB/s (steady) |
| NFS hot, **first read after cold**   | 250–600 MB/s |

The loop driver has a host-pagecache warm-up step on the first hot
read after cold population; bind-mount avoids it. On a 5 Gbps NIC,
that overhead is masked by the smaller absolute throughput; on a
beefier rig it dominates.

### Multi-thread scaling (bind-mount cache, RAID0 NVMe, 50 Gbps NIC)

`dd` worker per file, drop_caches between runs:

| concurrency        | aggregate    | per-worker |
|--------------------|-------------:|-----------:|
| qd=1               | 2.4 GB/s     | 2.4        |
| qd=4 same file     | 5.0 GB/s     | 1.25       |
| qd=8 same file     | 4.6 GB/s     | 0.6        |
| qd=4 distinct files| 6.3 GB/s     | 1.6        |
| **qd=8 distinct files** | **10.0 GB/s** | 1.25 |

fscache has **per-cookie locking** — same-file parallelism plateaus
~5 GB/s. Different-file parallelism scales to the underlying RAID0
ceiling (md2 RAID0 direct: 9.86 GB/s). LLM weight loaders that open
N safetensors shards in parallel benefit naturally.

### Layer ceilings (RAID0 NVMe ref)

| layer                    | qd=1     | qd=8     |
|--------------------------|---------:|---------:|
| md RAID0 direct          | 2.7 GB/s | 9.9 GB/s |
| xfs file direct          | 4.5 GB/s | 8.1 GB/s |
| loop-xfs file direct     | 2.3 GB/s | 3.0 GB/s |

Loop layer caps at ~3 GB/s regardless of underlying RAID — that's
why bind-mount wins on multi-NVMe rigs. Single-disk rigs have a
disk ceiling below the loop ceiling, so the gap doesn't matter.

The FUSE-based predecessor topped out at ~25–40 % of NVMe — the whole
point of this rewrite is staying on the kernel data path. Don't
reintroduce a FUSE/userspace data path.

## Testing

- Unit tests live inline (`#[cfg(test)] mod tests`) in `config.rs`,
  `proto/state.rs`, `proto/cmd.rs`. Run with `cargo test`.
- E2E (`tests/e2e/nfs-fscache.sh`) requires root, `cachefiles` module,
  `/var/cache/fscache` as its own mountpoint, and a reachable NFS
  server. Pass via env vars: `NFS_SERVER`, `NFS_EXPORT`, `TEST_FILE`.

## Branching

- Active branch: `fscache`. Main is `main`.
- Three commits on this branch tell the story: P0 prototype → P1+P2
  daemon+cull → P3+P4 packaging+e2e+docs.
- The earlier FUSE implementation was wiped in commit `76f5744`; do not
  resurrect it.

## When editing

- Keep dependencies minimal. The Cargo.toml deps are deliberate; see
  `docs/architecture.md` "Why no tokio / dashmap / parking_lot".
- The `inuse()` helper in `proto/cmd.rs` is `#[allow(dead_code)]` on
  purpose — kept for a future "skip-busy-before-trying-cull" optimization
  but redundant today since `cull` itself returns `Ok(false)` on `EBUSY`.
- When adding a new kernel command formatter, mirror the `cull`/`inuse`
  pattern: reject embedded `/`, `\0`, `\n`; map `EBUSY` to `Ok(false)`
  rather than an error if the kernel uses it as a soft signal.
- Don't add config fields without updating both `Config::validate` and
  the default `daemon.toml` in `packaging/etc/nfs-cachefs/`.
