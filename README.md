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
used because Ubuntu 24.04 ships with `CONFIG_CACHEFILES_ONDEMAND=n`
(verify with `grep CACHEFILES /boot/config-$(uname -r)`). See
[`docs/architecture.md`](docs/architecture.md) for the full rationale and
protocol details.

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
   # Or: loop-backed (validated path; xfs preferred — no extra mount opts needed)
   sudo truncate -s 100G /var/lib/nfs-cachefs.img
   sudo mkfs.xfs   /var/lib/nfs-cachefs.img
   sudo mount -o loop /var/lib/nfs-cachefs.img /var/cache/fscache
   ```

   The size of this filesystem is the upper bound on the cache. There is no
   `max_size` knob in `daemon.toml` — the cache fills the backing fs and
   the kernel triggers cull when free space drops below the `bcull` /
   `fcull` percentages (see `[limits]` in the config).

2. Edit `/etc/nfs-cachefs/daemon.toml` if you want non-default thresholds
   or a different cache directory.

3. Enable and start.

   ```sh
   sudo systemctl enable --now nfs-cachefs
   sudo systemctl status nfs-cachefs
   ```

4. Add `fsc` to your NFS mount options. Full fstab example (one logical
   line; backslashes shown only for typesetting):

   ```
   server:/export  /mnt/data  nfs  \
     vers=3,proto=tcp,fsc,timeo=60,retrans=2,nconnect=4,noatime,nodiratime,\
     nolock,nocto,actimeo=60,acregmax=3600,_netdev  0  0
   ```

   Or with systemd lazy-mount:

   ```
   server:/export  /mnt/data  nfs  \
     vers=3,proto=tcp,fsc,...,_netdev,x-systemd.automount,\
     x-systemd.idle-timeout=600  0  0
   ```

   The systemd unit ships with `Before=remote-fs-pre.target`, so
   `nfs-cachefs` starts before any standard NFS mount in `/etc/fstab`.
   **This ordering is load-bearing**: if the NFS mount races ahead of the
   daemon, `fsc` silently falls back to no-caching with no error logged.

5. Verify caching is wired up.

   ```sh
   cat /proc/fs/nfsfs/volumes        # FSC column = yes
   cat /proc/fs/fscache/caches       # state = A (active)
   cat /proc/fs/fscache/stats        # IO rd/wr counters move on access
   ```

## How the cache is sized and culled

There is no fixed-bytes cache limit. The cache fills the backing
filesystem; the kernel watches its free-space and inode percentages and
asks the daemon to start deleting LRU objects when space gets tight.
Three percentage thresholds control this (defaults shown):

| key   | default | meaning                                                  |
|-------|---------|----------------------------------------------------------|
| `run` | 10 %    | free-space target — daemon culls until free ≥ this       |
| `cull`| 7 %     | low-water mark — kernel signals daemon to start culling  |
| `stop`| 3 %     | hard floor — kernel refuses new caching below this       |

Each appears twice: `b*` for blocks (capacity), `f*` for inodes (file
count). Required ordering: `stop < cull < run ≤ 100`. A 100 G cache fs
with defaults effectively keeps cache size around 90–93 G and starts
shedding the oldest-by-atime objects when it hits 93 G. See `daemon.toml`
to retune. Cull walks the cache directory in batches of `cull.batch_size`
(default 1024) per pass.

## Performance

End-to-end check after the daemon is running and NFS is mounted with
`fsc`:

```sh
TESTFILE=/mnt/your-nfs/large-file
echo 3 | sudo tee /proc/sys/vm/drop_caches
sudo dd if="$TESTFILE" of=/dev/null bs=1M count=1024 status=progress  # cold
echo 3 | sudo tee /proc/sys/vm/drop_caches
sudo dd if="$TESTFILE" of=/dev/null bs=1M count=1024 status=progress  # hot
```

Validated on bare-metal Ubuntu 24.04 / kernel 6.8, WD Ultrastar DC SN640
PCIe Gen3 ×4 NVMe, loop-xfs cache image, NFSv3 over ~5 Gbps NIC, single
844 MB safetensors shard:

| read mode                          | throughput |
|------------------------------------|-----------:|
| cold (network → cache, 1 MiB bs)   | **695 MB/s** |
| hot, page cache (1 MiB bs)         | **1.8 GB/s** |
| hot, direct I/O (4 MiB bs)         | 1.2 GB/s   |

Cold throughput is network-limited. Hot pagecache reads exceed
local-xfs+pagecache for the same file (~730–970 MB/s) because NFS netfs
read-ahead is more aggressive than xfs's. Hot direct I/O at ~75 % of raw
xfs direct quantifies the cachefiles + loop layering overhead at about
25 %. The FUSE-based predecessor maxed out at 25–40 % of NVMe; the whole
point of this rewrite is staying on the kernel data path.

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
