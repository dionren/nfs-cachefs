# nfs-cachefs — agent notes

Rust userspace daemon for the Linux kernel's **fscache + cachefiles** path.
Drop-in replacement for the stagnant upstream `cachefilesd`. Targets
Ubuntu 24.04 / kernel 6.8.

Read first: `README.md`, `docs/architecture.md`. The plan that drove the
current rewrite is at `~/.claude/plans/fs-cache-squishy-umbrella.md` —
includes P0 verification results and the on-demand → traditional pivot.

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
  `CONFIG_CACHEFILES_ONDEMAND=n` (verify via `/boot/config-$(uname -r)`).
  `bind ondemand` is rejected by the kernel. Daemon role is configurator
  + cull driver, **not** per-request mediator.
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
cat /proc/fs/nfsfs/volumes        # FSC column = yes after fsc mount
cat /proc/fs/fscache/caches       # state column = A (active)
cat /proc/fs/fscache/stats        # IO rd/wr counters move on access
cat /proc/fs/fscache/cookies      # per-object cookies
journalctl -u nfs-cachefs -f      # daemon logs
```

## Performance baseline (already validated)

Loop-ext4 backing in QEMU on the user's NFSv3 setup:
- 1 GB cold read: ~850 MB/s (network-bound)
- 1 GB hot read: ~750–950 MB/s (loop ext4 / VM disk-bound)
- fscache `IO rd` += 1025 per 1 GB hot read (confirms cache-hit path)

On real PCIe 5.0 NVMe + DDR5 expect single-digit GB/s on hot reads. The
FUSE-based predecessor topped out at ~25–40% of NVMe; that's the reason
for the rewrite — don't reintroduce a FUSE/userspace data path.

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
