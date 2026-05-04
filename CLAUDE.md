# nfs-cachefs — agent notes

Rust userspace daemon for the Linux kernel's **fscache + cachefiles** path.
Drop-in replacement for the stagnant upstream `cachefilesd`. Minimum
supported platform is **Ubuntu 24.04 LTS / kernel 6.8**.

Read first: `README.md`, `docs/architecture.md`. The plan that drove the
current rewrite is at `~/.claude/plans/fs-cache-squishy-umbrella.md` —
includes P0 verification results and the on-demand → traditional pivot.

## Test machine

Primary remote test box. Credentials are out-of-band — never write the
password into the repo.

- Host: `cy-ah85009` at `10.20.100.9`, SSH as `root`.
- OS: Ubuntu 24.04.3 LTS, kernel `6.8.0-71-generic`.
- Hardware: 96 cores, 503 GiB RAM.
- NVMe: `/dev/nvme0n1` mounted at `/mnt/nvme` (xfs, 7.0 TiB, ~2 % used —
  plenty of room). Don't reformat the mount; carve a dedicated cache
  backing image under a subdir (`/mnt/nvme/nfs-cachefs-test/cache.img`)
  and loop-mount it (cachefiles requires the cache dir to be its own
  mountpoint).
- Cachefiles: `CONFIG_CACHEFILES=m` (built, not autoloaded —
  `modprobe cachefiles` before starting the daemon). 24.04 ships
  `CONFIG_CACHEFILES_ONDEMAND=n`, so traditional mode is the only path.
- `/mnt/llm-data` is **already mounted at boot** via fstab +
  `x-systemd.automount` (without `fsc`). To test, stop the automount
  unit and remount with `fsc` (see workflow). Don't edit fstab.
- Toolchain: **none, on purpose**. The test box is a deployment target,
  not a build host (see workflow below).

### Test workflow (build here, deploy there)

- **Always build on the dev host (where this repo lives), never on the
  test box.** Don't `cargo build` over SSH. Deploy the stripped release
  binary; no toolchain on the test box. Dev box and test box are both
  Ubuntu 24.04 / glibc 2.39, so binary compat is trivial.
  ```sh
  cargo build --release
  scp target/release/nfs-cachefs root@10.20.100.9:/usr/local/sbin/
  ```
- **Cache directory** — `/mnt/nvme/nfs-cachefs-test/cache.img` (loop image)
  mounted at `/var/cache/fscache`:
  ```sh
  mkdir -p /mnt/nvme/nfs-cachefs-test /var/cache/fscache
  truncate -s 100G /mnt/nvme/nfs-cachefs-test/cache.img
  mkfs.xfs /mnt/nvme/nfs-cachefs-test/cache.img
  mount -o loop /mnt/nvme/nfs-cachefs-test/cache.img /var/cache/fscache
  ```
- **NFS test mount sequence.** `/mnt/llm-data` is already up via systemd
  automount, **without** `fsc`. To test:
  ```sh
  modprobe cachefiles                          # load kernel module
  /usr/local/sbin/nfs-cachefs &                # start daemon (binds /dev/cachefiles)
  systemctl stop mnt-llm\\x2ddata.automount    # disable auto-remount
  umount /mnt/llm-data
  mount -t nfs -o vers=3,proto=tcp,fsc,timeo=60,retrans=2,nconnect=4,\
  noatime,nodiratime,nolock,nocto,actimeo=60,acregmax=3600 \
  10.20.66.203:/mnt/suanyun/llm-data /mnt/llm-data
  ```
  **Daemon must be bound before the fsc mount** — fsc on an unbound system
  silently falls back to "no caching" without erroring. After testing,
  `systemctl start mnt-llm\\x2ddata.automount` to restore the box default.
- **Module load**: `cachefiles.ko` is in-tree on 24.04 but not autoloaded.
  This is the *kernel module* — distinct from the legacy userspace
  `cachefilesd` daemon (the package this project replaces).

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
  otherwise. Test setup uses a loop-mounted ext4 image.
- **Only one daemon can hold `/dev/cachefiles`.** Open returns `EBUSY` if
  another holds it. There is **no `unbind` command** — closing the fd
  unbinds (kernel 6.8 returns `ENOTSUPP` on `unbind`). `Device::Drop`
  handles this; do not write `unbind`.
- **CWD requirement for `cull` / `inuse`.** Kernel resolves the object
  name via `current->fs`, so the daemon must `chdir(parent)` first. See
  `cull::run_pass`. This is why cull is single-threaded.
- **Limits ordering**: `stop < cull < run ≤ 100`, separately for `b*`
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

Real-hardware run on `cy-ah85009` (Ubuntu 24.04, kernel 6.8, WD
Ultrastar DC SN640 PCIe Gen3 x4 NVMe; loop-xfs cache image on host xfs;
NFSv3 against 10.20.66.203; single 844 MB safetensors shard):

| path | bs / mode | throughput |
|------|-----------|------------|
| raw `/dev/nvme0n1` direct | 4M direct | 1.9 GB/s |
| raw xfs file, direct      | 4M direct | 1.6 GB/s |
| raw xfs file, pagecache   | 1M after drop_caches | 730–970 MB/s |
| **NFS cold (network → cache)** | 1M after drop_caches | **695 MB/s** |
| **NFS hot (fscache hit, pagecache)** | 1M after drop_caches | **1.8 GB/s** |
| NFS hot, direct I/O       | 4M direct | 1.2 GB/s |

Hot pagecache reads exceed raw xfs+pagecache for the same file, likely
from NFS netfs read-ahead being more aggressive than xfs's. Hot direct
I/O at ~75 % of raw xfs direct quantifies the real cost of the
cachefiles + loop layering — about 25 % overhead, well within budget.
Cold is network-limited (~5 Gbps NIC).

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
