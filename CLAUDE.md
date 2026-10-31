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
- **Daemon must be bound before any `mount -o fsc`.** A mount made while
  no cache is bound shows `FSC=yes` but caches nothing, with no error
  logged (6.x source suggests the first cache bound later takes it over;
  unverified on the test box).

## Repo layout

```
src/main.rs           CLI, signals, tracing
src/config.rs         TOML schema + validation (ByteSize for max_size/extsize)
src/daemon.rs         poll loop, state read, health check, timers, heartbeat
src/planner.rs        when to cull and how much (pure, simulated in tests)
src/cull.rs           walk + candidate table + chdir + cull + truncate
src/quota.rs          max_size as a project quota (quotactl_fd)
src/fsattr.rs         FS_IOC_FS{GET,SET}XATTR: project ID, extsize hint
src/metrics.rs        node_exporter textfile writer
src/systemd_notify.rs sd_notify: READY, WATCHDOG, EXTEND_TIMEOUT_USEC
src/error.rs          thiserror enum (CacheDead, Interrupted, ...)
src/proto/cmd.rs      Kernel trait, Device, ConfigCmd::apply_and_bind, cull(), inuse()
src/proto/state.rs    parse the kernel state line
src/proto/mod.rs      re-exports + CACHEFILES_DEV constant
src/bin/probe.rs      diagnostic-only binary

packaging/{etc,systemd,share,install.sh}
tests/e2e/nfs-fscache.sh   needs root + real NFS server
```

About 3.5k lines including doc comments. No tokio / parking_lot /
dashmap on purpose — single fd, low event rate, single-threaded cull.
Don't add async runtime without a clear motivation.

## Build / run

```sh
cargo build --release       # → target/release/{nfs-cachefs,nfs-cachefs-probe}
sudo packaging/install.sh   # → /usr/sbin, /etc/nfs-cachefs, systemd, manpage
```

Toolchain: Rust ≥ 1.75 (stable). `.cargo/config.toml` makes cargo
resolve dependencies for `rust-version`, so `Cargo.lock` keeps building on
1.75 (check with `cargo +1.75 build --locked --all-targets`). Run
`cargo update` with a cargo ≥ 1.84 so the setting applies. Release profile
uses thin LTO, single codegen unit, stripped symbols → ~2 MB stripped
binary.

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
  caches nothing until a cache binds, silently. Systemd unit orders
  `Before=remote-fs-pre.target`.
- **Needs `CAP_SYS_ADMIN`** to write `/dev/cachefiles` (and for the
  `max_size` quota calls). Unit declares
  `CapabilityBoundingSet=CAP_SYS_ADMIN CAP_DAC_READ_SEARCH`.

## Protocol gotchas

- One command per `write()`. Don't batch. No newline terminator.
- `cull` and `inuse` returning `EBUSY` (kernel holds the object) is
  normal — "skip, try later" (`CullOutcome::Busy`); `ENOENT`/`ESTALE`
  mean the object is already gone (`CullOutcome::Gone`). See
  `proto/cmd.rs::cull`.
- Kernel state line is space-separated `k=v`; `cull` is decimal, others
  are **hex without `0x`**. Parser ignores unknown fields for forward
  compat (`proto/state.rs`).
- The state line's `b*` / `f*` are the kernel's **limits** (fixed at
  bind, in fs blocks / inodes), not free counts. Current free space comes
  from `statfs(cache_dir)` `f_bavail` / `f_ffree`, the same numbers the
  kernel compares against them.
- The kernel marks the device readable to wake the daemon up for state
  changes; we still poll with a ≤ 5 s timeout to drive timers + check
  the stop flag. `POLLOUT` stays asserted while `cull=1`, so the daemon
  polls for `POLLIN` only (polling `POLLOUT` spins whenever it waits).
- The kernel re-checks free space only on cache writes and daemon reads,
  so the daemon also reads the state on every poll timeout. Without that
  an idle cache never culls while other data fills a shared filesystem.
- `/proc/fs/fscache/caches` keeps a cache record in state `-` after its
  daemon exits while an `fsc` mount references it; a restart's bind with
  the same tag takes it over. Only `P`/`A`/`E`/`W` mean the tag is taken.
- **Size each cull pass on fresh statfs.** A pass frees only the
  shortfall to `brun` / `frun` (+ a small margin), counted in `st_blocks`
  (cache files are sparse; `st_size` overstates several-fold). XFS frees
  unlinked inodes in background workers — seconds for a fragmented cache
  file — so the pass opens each candidate first and, once the kernel has
  unlinked it (`st_nlink` 0), truncates it through that descriptor: the
  space is free when the pass ends. Only truncate an unlinked inode; the
  kernel refuses `cull` with EBUSY while it uses an object. Whatever is
  still freed in the background, `planner.rs` subtracts as pending for up
  to 60 s. Culling whole batches and re-reading state immediately emptied
  production caches on every cull episode (1.0.2); a fixed 30 s wait could
  still cull a shortfall twice (1.0.3).
- **Busy objects must not block the cull.** Objects that answer EBUSY (or
  fail) are set aside for 5 min (`CullTable::skip`); otherwise a batch of
  busy oldest objects comes back from every walk and nothing is ever
  culled.
- **A cache stopped by an I/O error (`E` in `/proc/fs/fscache/caches`)
  stays stopped** until the fd is closed and the cache bound again; every
  command returns EIO meanwhile. Setting a cache file's xattr failing with
  ENOSPC on a full disk is enough. The daemon checks every 10 s and on
  any cull EIO, and exits non-zero so systemd rebinds it.
- **Project quota + XFS renames:** XFS refuses renames between directories
  of different projects with EXDEV, and cachefiles turns a failed rename
  into its graveyard into a stopped cache. `quota.rs` therefore tags the
  whole tree before bind, never after.

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

- Unit tests live inline (`#[cfg(test)] mod tests`) in most modules. Run
  with `cargo test`. Cull pass logic is tested through `run_pass_with`
  with a fake kernel `cull`; the planner against a simulated filesystem
  whose frees show up late; the whole event loop (`daemon.rs`
  `runner_culls_one_episode_end_to_end`) against `FakeKernel`, which
  implements `proto::Kernel` with real unlinks and statfs. Production
  caches sit on XFS, so also run the tests with `TMPDIR` on an XFS mount
  when touching size accounting.
- Real-filesystem tests are `#[ignore]`d: `NFS_CACHEFS_TEST_XFS_DIR=<dir on
  XFS> cargo test -- --ignored extsize`, and, as root on a `prjquota`
  mount, the quota test: build with `cargo test --no-run`, then
  `sudo NFS_CACHEFS_TEST_PRJQUOTA_DIR=<mount> <test binary> --ignored quota`.
- E2E (`tests/e2e/nfs-fscache.sh`) requires root, `cachefiles` module,
  `/var/cache/fscache` as its own mountpoint, and a reachable NFS
  server. Pass via env vars: `NFS_SERVER`, `NFS_EXPORT`, `TEST_FILE`.

## Branching

- `main` carries the releases; each release is prepared on a
  `release/vX.Y.Z` branch and squash-merged. The original `fscache`
  branch holds the P0 → P4 history of the rewrite.
- The earlier FUSE implementation was wiped in commit `76f5744`; do not
  resurrect it.

## When editing

- Keep dependencies minimal. The Cargo.toml deps are deliberate; see
  `docs/architecture.md` "Why no tokio / dashmap / parking_lot".
- The `inuse()` helper in `proto/cmd.rs` is `#[allow(dead_code)]` on
  purpose — redundant today since `cull` itself reports
  `CullOutcome::Busy` on `EBUSY` and busy objects are then set aside.
- Code that talks to the kernel takes `&impl proto::Kernel`, not
  `&Device`, so tests can drive it with a fake.
- When adding a new kernel command formatter, mirror the `cull`/`inuse`
  pattern: reject embedded `/`, `\0`, `\n`; map `EBUSY` to `Ok(false)`
  rather than an error if the kernel uses it as a soft signal.
- Don't add config fields without updating both `Config::validate` and
  the default `daemon.toml` in `packaging/etc/nfs-cachefs/`.
