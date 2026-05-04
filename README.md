# nfs-cachefs

A Rust userspace daemon for the Linux kernel's **fscache + cachefiles**.
Drop-in replacement for the stagnant upstream `cachefilesd`, built and
tested on Ubuntu 24.04 LTS / kernel 6.8 (minimum supported).

The daemon's job is to make NFS reads transparently cache to local NVMe by
configuring the kernel's `cachefiles` backend and culling old objects when
free space drops. Applications see no extra mount point — NFS is mounted
with `-o fsc` and the kernel handles every read/write on the data path.

```
App → VFS → NFS client (-o fsc) → fscache → cachefiles.ko ↔ nfs-cachefs daemon
                                                           ↓
                                                  cache_dir on NVMe
```

This is **traditional cachefiles mode**: the daemon does configuration and
cull, not per-request mediation. On-demand mode (kernel ≥ 5.19) is not
used because Ubuntu 24.04 ships with `CONFIG_CACHEFILES_ONDEMAND=n`. See
the P0 verification finding in
`~/.claude/plans/fs-cache-squishy-umbrella.md`.

## Build

Requires Rust ≥ 1.75 (`rustup install stable`).

```sh
cargo build --release
# target/release/nfs-cachefs       — the daemon
# target/release/nfs-cachefs-probe — diagnostic: bind, log heartbeats, exit on signal
```

## Install

```sh
sudo packaging/install.sh
# Installs to /usr/sbin/, /etc/nfs-cachefs/, /lib/systemd/system/, /usr/share/man/man8/
```

Override paths via `PREFIX`, `SYSCONFDIR`, `SYSTEMD_UNIT_DIR`, `MANDIR`.

## Configure and run

1. Mount a dedicated filesystem at the cache directory. Cachefiles refuses
   to use a subdirectory of a host filesystem.

   ```sh
   # Example: dedicated NVMe partition
   sudo mkfs.xfs /dev/nvme0n1p1
   sudo mount /dev/nvme0n1p1 /var/cache/fscache
   # Or: loop-backed for testing
   sudo truncate -s 100G /var/lib/nfs-cachefs.img
   sudo mkfs.ext4 /var/lib/nfs-cachefs.img
   sudo mount -o loop,user_xattr /var/lib/nfs-cachefs.img /var/cache/fscache
   ```

2. Edit `/etc/nfs-cachefs/daemon.toml` if you want non-default thresholds
   or a different cache directory.

3. Enable and start.

   ```sh
   sudo systemctl enable --now nfs-cachefs
   sudo systemctl status nfs-cachefs
   ```

4. Add `fsc` to your NFS mount options. For the user's setup:

   ```
   10.20.66.203:/mnt/suanyun/llm-data  /mnt/llm-data  nfs  fsc,timeo=60,...  0 0
   ```

5. Verify caching is wired up.

   ```sh
   cat /proc/fs/nfsfs/volumes        # FSC column = yes
   cat /proc/fs/fscache/caches       # state = A (active)
   cat /proc/fs/fscache/stats        # IO rd/wr counters move on access
   ```

## Verification

End-to-end performance check after the daemon is running and NFS is
mounted with `fsc`:

```sh
TESTFILE=/mnt/your-nfs/large-file
echo 3 | sudo tee /proc/sys/vm/drop_caches
sudo dd if="$TESTFILE" of=/dev/null bs=1M count=1024 status=progress  # cold
echo 3 | sudo tee /proc/sys/vm/drop_caches
sudo dd if="$TESTFILE" of=/dev/null bs=1M count=1024 status=progress  # hot
```

The hot read should saturate the cache backing storage (≫ network
bandwidth on PCIe 5.0 NVMe). Validated on the user's NFSv3 setup with a
loop-ext4 cache: cold = 851 MB/s (network), hot = 739 MB/s (loop ext4 on
QEMU disk). On real NVMe expect multi-GB/s on hot reads.

## Diagnostics

```sh
# Daemon logs
journalctl -u nfs-cachefs -f

# Run the probe instead of the full daemon (lighter, no cull)
sudo /usr/sbin/nfs-cachefs-probe --cache-dir /var/cache/fscache --tag probe

# Kernel-side
cat /proc/fs/fscache/stats
cat /proc/fs/fscache/caches
cat /proc/fs/fscache/cookies
```

## License

MIT — see `LICENSE`.
