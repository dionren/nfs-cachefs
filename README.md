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

1. Make `cache_dir` its own mountpoint. Cachefiles refuses a path that
   isn't a mount root (`mnt->mnt_root == dentry`). Three setups satisfy
   it; pick the one that matches your disk layout.

   **Recommended — self-bind a subdirectory** of an existing NVMe xfs
   (no loop driver, no repartitioning):

   ```sh
   sudo mkdir -p /mnt/nvme/cache && sudo chmod 0700 /mnt/nvme/cache
   sudo mount --bind /mnt/nvme/cache /mnt/nvme/cache
   # persist across reboot:
   echo '/mnt/nvme/cache  /mnt/nvme/cache  none  bind,x-systemd.requires=mnt-nvme.mount  0 0' \
     | sudo tee -a /etc/fstab
   ```

   **Dedicated partition / logical volume** if you have spare disk:

   ```sh
   sudo mkfs.xfs /dev/nvme0n1p1
   sudo mount /dev/nvme0n1p1 /var/cache/fscache
   ```

   **Loop-backed image** if you need a fixed-size container on shared fs:

   ```sh
   sudo truncate -s 100G /var/lib/nfs-cachefs.img
   sudo mkfs.xfs   /var/lib/nfs-cachefs.img
   sudo mount -o loop /var/lib/nfs-cachefs.img /var/cache/fscache
   ```

   On multi-NVMe rigs the bind-mount option avoids ~3 GB/s of loop
   driver overhead — see [Performance](#performance) for numbers.

   The capacity of this filesystem (or, for bind-mount, the parent fs
   minus other content) is the upper bound on the cache. There is no
   `max_size` knob in `daemon.toml` — the cache fills the backing fs and
   the kernel triggers cull when free space drops below the `bcull` /
   `fcull` percentages (see `[limits]` in the config).

2. Edit `/etc/nfs-cachefs/daemon.toml` if you want non-default thresholds
   or a different cache directory. If you run under the packaged systemd
   unit and change `cache_dir` after installation, also add a drop-in that
   sets `ReadWritePaths=` to the same path; the shipped unit keeps the daemon
   confined to `/var/cache/fscache` by default.

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

### Single-thread baseline

Two reference rigs, both Ubuntu 24.04 / kernel 6.8, NFSv3:

| rig | NIC | cache backend | cold | hot |
|-----|-----|---------------|------|-----|
| 2× Intel D7-P5510 in md RAID0  | 50 Gbps | **bind-mount** xfs | 0.7–1.2 GB/s | **1.4–1.8 GB/s** |
| WD Ultrastar SN640 single NVMe | 5 Gbps  | loop-xfs image     | 695 MB/s     | 1.8 GB/s        |

Cold is network-limited on both. Hot equals or exceeds raw xfs
+pagecache for the same file because NFS netfs read-ahead is more
aggressive than xfs's. Bind-mount avoids a "first-hot-after-cold"
warm-up that loop images suffer (250–600 MB/s on the first hot
read until the host pagecache has the loop image's blocks).

### Multi-thread scaling (bind-mount, RAID0 NVMe)

Single-thread is bottlenecked by `netfs`/`cachefiles` per-cookie locking
and CPU memcpy, **not** disk. Open multiple readers on different files
to scale up:

| concurrency             | aggregate | comment |
|-------------------------|----------:|---------|
| 1 reader, 1 file        | 2.4 GB/s  | baseline |
| 4 readers, same file    | 5.0 GB/s  | per-cookie lock plateaus here |
| 4 readers, 4 files      | 6.3 GB/s  | scales across files |
| **8 readers, 4 files**  | **10 GB/s** | saturates the RAID0 ceiling |

LLM inference loaders (vLLM, sglang) that open N safetensors shards
in parallel hit ~10 GB/s on this rig directly. The FUSE-based
predecessor maxed out at 25–40 % of NVMe; this rewrite stays on the
kernel data path.

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
