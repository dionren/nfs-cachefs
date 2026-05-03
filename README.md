# nfs-cachefs

A Rust userspace daemon for the Linux kernel's **fscache + cachefiles
on-demand mode**, intended to replace the stagnant upstream `cachefilesd`
on modern kernels (5.19+; primarily targeting Ubuntu 24.04 / kernel 6.8).

The daemon's job is to make NFS reads transparently cache to local NVMe by
configuring the kernel's `cachefiles` backend and managing free space.
Applications see no extra mountpoint — NFS is mounted with `-o fsc` and the
kernel handles the rest, with this daemon as the userspace policy/cull side.

```
App → VFS → NFS client (-o fsc) → fscache → cachefiles.ko ↔ nfs-cachefs daemon
                                                           ↓
                                                         /var/cache/fscache (NVMe)
```

## Status

**P0 — feasibility prototype.** The current binary `nfs-cachefs-probe` only
verifies that the kernel routes NFS-via-fscache through the on-demand
protocol. It is not a production daemon. The full daemon is being built in
phases per `~/.claude/plans/fs-cache-squishy-umbrella.md`.

## Building

Requires Rust ≥ 1.75.

```sh
cargo build --release
# Binaries at target/release/{nfs-cachefs, nfs-cachefs-probe}
```

## P0 verification

This proves the on-demand path works for NFS on your kernel before the rest
of the daemon is built.

### Prerequisites

- Linux ≥ 5.19 (Ubuntu 24.04's 6.8 is fine)
- `cachefiles` kernel module
- A dedicated mountpoint on NVMe for the cache (here: `/var/cache/fscache`)
- An NFS server you can mount

```sh
# Ensure cachefiles is loadable
sudo modprobe cachefiles
ls /dev/cachefiles                       # should exist

# Cache directory: must be a real mountpoint, not a subdirectory
# (cachefiles refuses to use a subdir of a mounted fs unless it's on its own fs)
sudo mkdir -p /var/cache/fscache
# If /var/cache is not its own mount, either bind-mount or use a different path:
#   sudo mount --bind /fast-nvme/fscache /var/cache/fscache
```

### Run the probe

```sh
sudo RUST_LOG=debug ./target/release/nfs-cachefs-probe \
  --cache-dir /var/cache/fscache \
  --tag nfsprobe
```

Leave this running. In another terminal:

```sh
sudo mount -t nfs -o fsc,vers=4.2 nfs-server:/export /mnt/nfs
cat /proc/fs/nfsfs/volumes               # FSC column should be yes

# Cold read (network bandwidth bound)
sudo dd if=/mnt/nfs/bigfile of=/dev/null bs=1M count=1024 status=progress

# Drop page cache, force re-read from disk-backed fscache
echo 3 | sudo tee /proc/sys/vm/drop_caches

# Hot read (should saturate NVMe if fscache populated)
sudo dd if=/mnt/nfs/bigfile of=/dev/null bs=1M count=1024 status=progress

# Inspect
cat /proc/fs/fscache/stats | grep -E '^(IO|Pages|Ops)'
ls -la /var/cache/fscache/
```

### Decision gate

- ✅ **Pass:** Probe logs `OPEN` events, cache files appear under
  `/var/cache/fscache/`, second `dd` is faster than network bandwidth →
  on-demand mode is the right architecture; proceed to P1 (real daemon).
- ❌ **Fail:** No `OPEN` events / `bind ondemand` write returns `EINVAL` /
  cache dir stays empty → NFS+on-demand is not viable on this kernel; fall
  back to traditional `bind` mode (plan revision required).

## License

MIT — see `LICENSE`.
