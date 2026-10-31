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

## Install

A single end-to-end installer (`packaging/install.sh`, also published as
`install.sh` in each GitHub release) handles everything: environment
checks, file installation, the bind-mounted cache directory, the cached
NFS mount in `/etc/fstab`, the systemd unit + drop-in, module autoload,
and starting the daemon. It works in three modes — auto-detected, first
match wins:

1. **In-source build** — `sudo packaging/install.sh` from a built repo
   (uses `target/release/*` directly).
2. **Offline tarball** — `sudo ./install.sh` with the release tarball
   `nfs-cachefs-linux-amd64.tar.gz` next to the script (or
   `NFSCACHEFS_TARBALL=/path/to/tarball.tar.gz`).
3. **Online** — fetched from GitHub releases.

### Online (Ubuntu 24.04 / x86_64)

Interactive — three prompts (defaults shown):

```sh
curl -fsSL https://github.com/dionren/nfs-cachefs/releases/latest/download/install.sh | sudo bash
```

| prompt           | default                      |
|------------------|------------------------------|
| Cache directory  | `/mnt/nvme/nfs-cachefs`      |
| Mount directory  | `/mnt/llm-data`              |
| NFS endpoint     | *(required, `server:/export`)* |

Non-interactive — pass values via env vars (also the recommended form for
**upgrades**: rerun this on an existing install and the script preserves
your `daemon.toml` customizations, rewrites the fstab line in place, and
restarts the daemon without dropping the live mount):

```sh
curl -fsSL https://github.com/dionren/nfs-cachefs/releases/latest/download/install.sh \
  | sudo CACHE_DIR=/mnt/nvme/nfs-cachefs \
         MOUNT_DIR=/mnt/llm-data \
         NFS_ENDPOINT=nfs.example.com:/srv/share \
         NFSCACHEFS_YES=1 \
         bash
```

### Offline / air-gapped

Download the script and the tarball ahead of time, drop them in the same
directory, then run as root:

```sh
RELEASE=https://github.com/dionren/nfs-cachefs/releases/latest/download
curl -fsSLO "$RELEASE/install.sh"
curl -fsSLO "$RELEASE/nfs-cachefs-linux-amd64.tar.gz"
curl -fsSLO "$RELEASE/nfs-cachefs-linux-amd64.tar.gz.sha256"   # optional, verified if present
chmod +x install.sh
sudo ./install.sh
```

Or point at a tarball anywhere with `NFSCACHEFS_TARBALL=/path/to/tar.gz sudo ./install.sh`.

### Build from source

```sh
cargo build --release      # Rust ≥ 1.75
sudo packaging/install.sh  # auto-detects the in-source build
```

### All env knobs

The curl-piped non-interactive form above is the minimum; below are all
knobs the script honors. They work the same whether the script is piped
from curl, run from disk (`./install.sh`), or run from the repo
(`packaging/install.sh`):

```sh
sudo CACHE_DIR=/mnt/nvme/nfs-cachefs \
     MOUNT_DIR=/mnt/llm-data \
     NFS_ENDPOINT=nfs.example.com:/srv/share \
     NFS_RW=1 NFS_NCONNECT=4 NFS_VERS=3 \
     NFSCACHEFS_YES=1 \
     ./install.sh
```

| env var               | default                  | notes                              |
|-----------------------|--------------------------|------------------------------------|
| `CACHE_DIR`           | `/mnt/nvme/nfs-cachefs`  | self-bind-mounted                  |
| `MOUNT_DIR`           | `/mnt/llm-data`          | NFS mount target                   |
| `NFS_ENDPOINT`        | *(required)*             | `server:/export`                   |
| `NFS_RW`              | `1`                      | `0` for read-only                  |
| `NFS_NCONNECT`        | `4`                      | TCP connections (1..16)            |
| `NFS_VERS`            | `3`                      | `3`, `4`, `4.1`, `4.2`             |
| `NFSCACHEFS_TARBALL`  | *(auto-detect / online)* | force a specific tarball           |
| `NFSCACHEFS_RELEASE`  | `latest`                 | pin a release tag                  |
| `NFSCACHEFS_YES`      | unset                    | skip confirmation prompts          |
| `NFSCACHEFS_NO_START` | unset                    | install only; don't start daemon   |

### What the installer actually does

The installer is idempotent and preserves existing entries:

1. Writes the binaries (`/usr/sbin/`), config (`/etc/nfs-cachefs/daemon.toml`),
   unit file (`/lib/systemd/system/`), and a drop-in
   (`/etc/systemd/system/nfs-cachefs.service.d/local.conf`) that pins
   `ReadWritePaths=` and `RequiresMountsFor=` to your `cache_dir`
   (upgrades keep an existing drop-in, so hand edits stick). A
   running daemon is **not** stopped here — the kernel keeps the old
   binary mmapped, and step 5 picks up the new code via `restart`.
2. Adds `cachefiles` to `/etc/modules-load.d/` so the kernel module is
   loaded on boot.
3. Creates `cache_dir` 0700 and self-bind-mounts it (cachefiles
   requires `mnt->mnt_root == dentry`); appends an fstab `bind` entry
   with `x-systemd.requires=` pointing at the parent mount unit.
4. Writes / **replaces** the fstab line at `MOUNT_DIR`. If a non-`fsc`
   NFS entry is already there, it's commented out and the new line is
   inserted in place; backups land at `/etc/fstab.bak.<timestamp>`.
   The new options:
   `auto,_netdev,fsc,nosharecache,vers=$NFS_VERS,proto=tcp,nconnect=$NFS_NCONNECT,timeo=60,retrans=2,noatime,nodiratime,nolock,nocto,actimeo=60,acregmax=3600,$rw_or_ro`
5. `modprobe cachefiles`, `daemon-reload`, then either `enable --now`
   (fresh install) or `restart` (upgrade) on `nfs-cachefs`. Finally
   `mount $MOUNT_DIR` if it isn't already mounted.
6. Verifies: `/proc/fs/nfsfs/volumes` should now show `FSC=yes` for the
   cached export, and `/proc/fs/fscache/caches` shows state `A`.

The unit's `Before=remote-fs-pre.target` ordering keeps the daemon
ahead of any standard NFS mount at boot — that's load-bearing: a mount
made while no cache is bound still shows `FSC=yes` but caches nothing,
and nothing is logged. (Per the kernel source, 6.x hands such a mount to
the first cache bound afterwards; this has not been verified on the test
box, so keep the ordering.) The fstab line intentionally omits
`x-systemd.requires=nfs-cachefs.service`: with that option set, every
daemon restart cascades to a forced unmount, and the subsequent
re-mount can land on a stale fscache superblock (visible in
`/proc/fs/nfsfs/volumes` as a duplicate FSID line with `FSC=no`).
Boot-time ordering alone is sufficient; for clean live upgrades use
`systemctl restart nfs-cachefs` (the live mount keeps `fsc` because
the kernel re-attaches cookies when the cache rebinds).

### Manual setup (without the installer)

If you'd rather do each step yourself, the recipe is the same as the
installer's: `cache_dir` must be its own mountpoint (a self-bind on an
xfs/ext4 NVMe is the fast path; a dedicated partition or loop image
also works), the NFS mount needs `fsc,nosharecache,_netdev`, and the
daemon must start before the mount. Quick check after wiring it up:

```sh
cat /proc/fs/nfsfs/volumes        # FSC column = yes
cat /proc/fs/fscache/caches       # state = A (active)
cat /proc/fs/fscache/stats        # IO rd/wr counters move on access
```

### Uninstall

```sh
sudo systemctl disable --now nfs-cachefs
sudo umount /mnt/llm-data /mnt/nvme/nfs-cachefs 2>/dev/null || true
sudo rm -f  /usr/sbin/nfs-cachefs /usr/sbin/nfs-cachefs-probe
sudo rm -f  /usr/share/man/man8/nfs-cachefs.8
sudo rm -rf /etc/nfs-cachefs /etc/systemd/system/nfs-cachefs.service.d
sudo rm -f  /lib/systemd/system/nfs-cachefs.service
sudo rm -f  /etc/modules-load.d/cachefiles.conf
# then remove the two fstab entries the installer added (or restore from /etc/fstab.bak.*)
```

## How the cache is sized and culled

The kernel protocol has no fixed-bytes cache limit. The cache fills the
backing filesystem; the kernel watches its free-space and inode
percentages and asks the daemon to start deleting LRU objects when space
gets tight. Three percentage thresholds control this (defaults shown):

| key   | default | meaning                                                  |
|-------|---------|----------------------------------------------------------|
| `run` | 10 %    | free-space target — daemon culls until free ≥ this       |
| `cull`| 7 %     | low-water mark — kernel signals daemon to start culling  |
| `stop`| 3 %     | hard floor — kernel refuses new caching below this       |

Each appears twice: `b*` for blocks (capacity), `f*` for inodes (file
count). Required ordering: `stop < cull < run < 100`. A 100 G cache fs
with defaults effectively keeps cache size around 90–93 G and starts
shedding the oldest-by-atime objects when it hits 93 G. See `daemon.toml`
to retune.

Each cull pass frees what is missing to get back to `run` (plus a small
margin), counting the space files really occupy, not their sparse size.
Culled files are truncated through a descriptor the daemon holds, so
their space is free — and visible to the kernel — when the pass ends,
not seconds later when XFS gets to it in the background. Objects the
kernel reports in use are set aside for a few minutes instead of being
retried on every pass, and a pass that can free nothing backs off (1 s,
doubling up to 60 s) instead of spinning.

The `heartbeat` log line shows where the cache stands: `bavail` / `ffree`
are the current free blocks / inodes, `bcull` / `brun` (and `fcull` /
`frun`) the limits they are compared against.

### Cache size budget on a shared disk (`max_size`)

The percentages are of the whole filesystem, so on a volume shared with
other data (container images, swap files) that data counts too: the cache
grows until the volume is 93 % full, gives space back as the other data
grows, and is emptied if the other data alone keeps free space below
`run`. To cap the cache itself, give it a budget in `daemon.toml`:

```toml
max_size = "3T"
```

Before binding, the daemon puts the cache tree under an XFS or ext4
**project quota** of that size. statfs on the cache directory then
reports the budget as its size and the smaller of "budget left" and the
real free space as available, so the percentages apply to the budget —
the cache stays between roughly 2.7 and 2.8 T here — and the cache still
shrinks when the shared filesystem itself runs low.

- The filesystem must be mounted with project quotas (`prjquota`; XFS
  also accepts `pquota`). The daemon refuses to start otherwise.
- `quota_project_id` (default `1852269427`) must not be used by anything
  else; check with `xfs_quota -x -c 'report -p' <mountpoint>`. Docker
  hands out small IDs from 2 upward.
- The first start tags the existing cache files, which takes a moment on a
  large cache; later starts only check. A new `max_size` takes effect at
  the next restart.
- To drop the budget, remove `max_size` and clear the limit:
  `xfs_quota -x -c 'limit -p bhard=0 1852269427' <mountpoint>`.

### XFS extent size hint (`extsize`)

The kernel writes cache files out of order as NFS reads complete, so a
large file can end up in hundreds of thousands of extents, which makes it
slow to delete and costly to map. `extsize = "4M"` sets an XFS extent size
hint that new cache files inherit, so XFS allocates them in aligned 4 MiB
chunks. A partially read file may then occupy up to one chunk more per
hole. Existing files keep their layout.

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

## Metrics

With

```toml
[metrics]
textfile = "/run/nfs-cachefs/nfs-cachefs.prom"
```

the daemon rewrites a Prometheus text file every 15 s for node_exporter's
textfile collector: point it at the directory
(`--collector.textfile.directory=/run/nfs-cachefs`) or symlink the file
into an existing textfile directory. The unit makes `/run/nfs-cachefs`
writable; another directory needs a `ReadWritePaths=` line in the
drop-in. The file is removed when the daemon stops.

| metric | meaning |
|--------|---------|
| `nfs_cachefs_cache_active`, `nfs_cachefs_cache_io_error` | kernel state of the cache: active (A) / stopped by an I/O error (E) |
| `nfs_cachefs_culling` | 1 while the kernel asks for culling |
| `nfs_cachefs_fs_size_bytes`, `nfs_cachefs_fs_avail_bytes` | the cache filesystem (or the `max_size` budget) as statfs reports it |
| `nfs_cachefs_limit_bytes{limit="run"\|"cull"\|"stop"}` | the kernel's thresholds |
| `nfs_cachefs_cache_objects`, `nfs_cachefs_cache_allocated_bytes` | cache size, from a walk of the cache every 5 min |
| `nfs_cachefs_quota_used_bytes`, `nfs_cachefs_quota_limit_bytes` | the same from the project quota, with `max_size` |
| `nfs_cachefs_culled_objects_total`, `nfs_cachefs_culled_bytes_total` | what culling removed |
| `nfs_cachefs_cull_passes_total`, `nfs_cachefs_cull_passes_without_progress_total`, `nfs_cachefs_cull_busy_skips_total`, `nfs_cachefs_cull_errors_total` | cull activity |
| `nfs_cachefs_fscache_stat{section,field}` | every counter of `/proc/fs/fscache/stats`, e.g. `section="IO",field="rd"` (reads served from the cache) |

Worth alerting on: `nfs_cachefs_cache_io_error == 1` (left in the file
when the daemon exits to have the cache bound again, until the restarted
daemon replaces it), the series disappearing, and
`increase(nfs_cachefs_cull_passes_without_progress_total[15m]) > 0`.

## Service behavior

- `Type=notify`: the unit is up once the cache is bound, and starts before
  `remote-fs-pre.target`, so fsc mounts at boot find the cache.
- `Restart=always`, with the delay growing from 2 s to 2 min: the daemon
  comes back after any exit systemd did not ask for, a stray SIGTERM
  included.
- When the kernel stops the cache after an I/O error (state `E` in
  `/proc/fs/fscache/caches`; a full disk failing a cache metadata write is
  enough), it caches nothing more until the cache is bound again. The
  daemon checks every 10 s and exits so that systemd restarts it.
- `WatchdogSec=2min`: a hung daemon is restarted.
- Stopping can take a while on a busy shared disk: closing
  `/dev/cachefiles` makes the kernel sync the cache filesystem
  (`TimeoutStopSec=90s`).

## Diagnostics

```sh
# Daemon logs
journalctl -u nfs-cachefs -f

# Run the probe instead of the full daemon (lighter, no cull). Stop the
# daemon first: only one process can hold /dev/cachefiles.
sudo /usr/sbin/nfs-cachefs-probe --cache-dir /var/cache/fscache --tag probe

# Kernel-side
cat /proc/fs/fscache/stats
cat /proc/fs/fscache/caches
cat /proc/fs/fscache/cookies
```

## License

MIT — see `LICENSE`.
