#!/usr/bin/env bash
# nfs-cachefs remote installer.
#
# One-line install (interactive):
#   curl -fsSL https://github.com/dionren/nfs-cachefs/releases/latest/download/install.sh | sudo bash
#
# Non-interactive install (env vars override prompts; NFS_ENDPOINT is required):
#   curl -fsSL https://github.com/dionren/nfs-cachefs/releases/latest/download/install.sh \
#     | sudo CACHE_DIR=/mnt/nvme/nfs-cachefs \
#            MOUNT_DIR=/mnt/llm-data \
#            NFS_ENDPOINT=nfs.example.com:/srv/share \
#            NFSCACHEFS_YES=1 \
#            bash
#
# Pin to a specific release (default: latest):
#   NFSCACHEFS_RELEASE=v1.0.0 curl ... | sudo bash
#
# Skip systemd start (config only):
#   NFSCACHEFS_NO_START=1 curl ... | sudo bash

set -euo pipefail

# ─── Configuration ────────────────────────────────────────────────────────
REPO="dionren/nfs-cachefs"
RELEASE="${NFSCACHEFS_RELEASE:-latest}"
ASSET="nfs-cachefs-linux-amd64.tar.gz"

DEFAULT_CACHE_DIR="/mnt/nvme/nfs-cachefs"
DEFAULT_MOUNT_DIR="/mnt/llm-data"
# NFS endpoint has no default: it's site-specific and we don't ship a placeholder
# that could route anywhere. Empty default → user must enter a value.
DEFAULT_NFS_ENDPOINT=""

# ─── I/O helpers ──────────────────────────────────────────────────────────
if [[ -t 1 || -e /dev/tty ]]; then
    BOLD=$'\033[1m'; RED=$'\033[31m'; GREEN=$'\033[32m'
    YELLOW=$'\033[33m'; DIM=$'\033[2m'; RESET=$'\033[0m'
else
    BOLD='' RED='' GREEN='' YELLOW='' DIM='' RESET=''
fi

err()  { printf '%serror:%s %s\n' "$RED$BOLD"    "$RESET" "$*" >&2; }
warn() { printf '%swarn:%s  %s\n' "$YELLOW$BOLD" "$RESET" "$*" >&2; }
info() { printf '%s==>%s    %s\n' "$GREEN$BOLD"  "$RESET" "$*"; }
step() { printf '\n%s── %s ──%s\n' "$BOLD" "$*" "$RESET"; }

ask() {
    # ask VARNAME DEFAULT MSG
    # Empty DEFAULT means "required" — no-tty path errors out, tty path re-prompts.
    local var=$1 default=$2 msg=$3 input=""
    if [[ -n "${!var:-}" ]]; then
        info "$msg (env $var): ${!var}"
        return 0
    fi
    if [[ ! -e /dev/tty ]]; then
        if [[ -n "$default" ]]; then
            printf -v "$var" '%s' "$default"
            info "$msg (default, no tty): $default"
            return 0
        fi
        err "$msg is required; rerun with env var $var set"
        exit 1
    fi
    while :; do
        if [[ -n "$default" ]]; then
            printf '%s%s%s [%s%s%s]: ' "$BOLD" "$msg" "$RESET" "$DIM" "$default" "$RESET" >/dev/tty
        else
            printf '%s%s%s: ' "$BOLD" "$msg" "$RESET" >/dev/tty
        fi
        IFS= read -r input </dev/tty || true
        [[ -z "$input" ]] && input=$default
        if [[ -n "$input" ]]; then
            printf -v "$var" '%s' "$input"
            return 0
        fi
        warn "value required (no default)"
    done
}

confirm() {
    # confirm MSG default(Y|n)
    local msg=$1 default=${2:-Y}
    [[ -n "${NFSCACHEFS_YES:-}" ]] && return 0
    [[ ! -e /dev/tty ]] && return 0
    local prompt="[Y/n]"
    [[ "$default" == "n" ]] && prompt="[y/N]"
    printf '%s%s%s %s ' "$BOLD" "$msg" "$RESET" "$prompt" >/dev/tty
    local reply=""
    IFS= read -r reply </dev/tty || true
    [[ -z "$reply" ]] && reply=$default
    [[ "$reply" =~ ^[Yy] ]]
}

cleanup() { [[ -n "${WORKDIR:-}" && -d "$WORKDIR" ]] && rm -rf "$WORKDIR"; }
trap cleanup EXIT

# ─── Environment check ───────────────────────────────────────────────────
need_root() {
    if [[ $EUID -ne 0 ]]; then
        err "must be run as root (use sudo)"
        exit 1
    fi
}

check_environment() {
    step "Environment check"
    need_root

    local arch; arch=$(uname -m)
    if [[ "$arch" != "x86_64" ]]; then
        err "unsupported architecture: $arch (only x86_64 builds are released)"
        exit 1
    fi
    info "arch: $arch"

    local kver kmajor kminor
    kver=$(uname -r)
    if [[ "$kver" =~ ^([0-9]+)\.([0-9]+) ]]; then
        kmajor=${BASH_REMATCH[1]}
        kminor=${BASH_REMATCH[2]}
        if (( kmajor < 6 || (kmajor == 6 && kminor < 8) )); then
            warn "kernel $kver is older than 6.8 (the tested baseline)"
            confirm "Continue anyway?" n || exit 1
        fi
    fi
    info "kernel: $kver"

    if ! grep -q '^ID=ubuntu' /etc/os-release 2>/dev/null; then
        warn "this installer is tested only on Ubuntu 24.04. Other distros may work but YMMV."
    fi

    if ! modinfo cachefiles >/dev/null 2>&1; then
        err "cachefiles kernel module not available in /lib/modules/$kver"
        err "  install the matching linux-modules package and retry"
        exit 1
    fi
    info "cachefiles module: present"

    if [[ -e /dev/cachefiles ]] && command -v fuser >/dev/null 2>&1 \
        && fuser -s /dev/cachefiles 2>/dev/null; then
        err "/dev/cachefiles is held by another process"
        err "  stop existing daemons first: systemctl stop nfs-cachefs cachefilesd"
        exit 1
    fi

    if ! command -v mount.nfs >/dev/null 2>&1; then
        warn "nfs-common not installed; installing via apt"
        DEBIAN_FRONTEND=noninteractive apt-get update -qq
        DEBIAN_FRONTEND=noninteractive apt-get install -y -q nfs-common
    fi
    info "nfs client: ready"

    local missing=() c
    for c in curl tar systemctl awk grep mount findmnt mountpoint sha256sum systemd-escape sed install stat; do
        command -v "$c" >/dev/null 2>&1 || missing+=("$c")
    done
    if (( ${#missing[@]} > 0 )); then
        err "missing required commands: ${missing[*]}"
        exit 1
    fi
    info "all environment checks passed"
}

# ─── Collect inputs ──────────────────────────────────────────────────────
collect_inputs() {
    step "Configuration"
    ask CACHE_DIR    "$DEFAULT_CACHE_DIR"    "Cache directory (will be its own mountpoint)"
    ask MOUNT_DIR    "$DEFAULT_MOUNT_DIR"    "Mount directory for the cached NFS"
    ask NFS_ENDPOINT "$DEFAULT_NFS_ENDPOINT" "NFS endpoint (server:/export)"

    [[ "$CACHE_DIR" == /* ]] || { err "CACHE_DIR must be absolute"; exit 1; }
    [[ "$MOUNT_DIR" == /* ]] || { err "MOUNT_DIR must be absolute"; exit 1; }
    [[ "$NFS_ENDPOINT" == *:/* ]] || { err "NFS_ENDPOINT must be 'server:/export'"; exit 1; }
    [[ "$CACHE_DIR" != "$MOUNT_DIR" ]] || { err "CACHE_DIR and MOUNT_DIR must differ"; exit 1; }

    local parent; parent=$(dirname "$CACHE_DIR")
    if [[ ! -d "$parent" ]]; then
        warn "parent directory $parent does not exist"
        confirm "Create $parent?" Y || { err "aborted"; exit 1; }
        install -d -m 0755 "$parent"
    fi

    local fstype
    fstype=$(findmnt -no FSTYPE -T "$parent" 2>/dev/null || true)
    case "$fstype" in
        xfs|ext4|btrfs) ;;
        '') warn "could not detect fs type of $parent" ;;
        *)  warn "parent fs is $fstype; cachefiles needs xattr support (xfs or ext4 recommended)" ;;
    esac

    info "cache directory: $CACHE_DIR  (fs: ${fstype:-unknown})"
    info "mount target:    $MOUNT_DIR"
    info "NFS endpoint:    $NFS_ENDPOINT"
    confirm "Proceed with installation?" Y || { err "aborted by user"; exit 1; }
}

# ─── Download ────────────────────────────────────────────────────────────
download_release() {
    step "Downloading release ($RELEASE)"
    WORKDIR=$(mktemp -d /tmp/nfs-cachefs-install.XXXXXX)

    local base
    if [[ "$RELEASE" == "latest" ]]; then
        base="https://github.com/$REPO/releases/latest/download"
    else
        base="https://github.com/$REPO/releases/download/$RELEASE"
    fi

    info "fetching $base/$ASSET"
    curl -fsSL --retry 3 -o "$WORKDIR/$ASSET" "$base/$ASSET"

    if curl -fsSL --retry 3 -o "$WORKDIR/$ASSET.sha256" "$base/$ASSET.sha256" 2>/dev/null; then
        (cd "$WORKDIR" && sha256sum -c "$ASSET.sha256")
        info "checksum verified"
    else
        warn "no checksum file available; skipping verification"
    fi

    info "extracting tarball"
    tar xzf "$WORKDIR/$ASSET" -C "$WORKDIR"
    EXTRACT="$WORKDIR/nfs-cachefs"
    [[ -x "$EXTRACT/sbin/nfs-cachefs" ]] || { err "tarball missing nfs-cachefs binary"; exit 1; }
    if [[ -f "$EXTRACT/VERSION" ]]; then
        info "downloaded version: $(cat "$EXTRACT/VERSION")"
    fi
}

# ─── fstab helpers ───────────────────────────────────────────────────────
fstab_has_target() {
    # fstab_has_target TARGET FSTYPE
    awk -v t="$1" -v ft="$2" '
        $0 ~ /^[[:space:]]*#/ {next}
        NF >= 3 && $2 == t && $3 == ft { found=1 }
        END { exit !found }
    ' /etc/fstab
}

fstab_append() {
    # fstab_append COMMENT LINE
    local comment=$1 line=$2
    if [[ -s /etc/fstab && "$(tail -c1 /etc/fstab)" != $'\n' ]]; then
        printf '\n' >> /etc/fstab
    fi
    printf '\n# %s\n%s\n' "$comment" "$line" >> /etc/fstab
}

# ─── Install files ───────────────────────────────────────────────────────
install_files() {
    step "Installing files"

    install -m 0755 "$EXTRACT/sbin/nfs-cachefs"       /usr/sbin/nfs-cachefs
    install -m 0755 "$EXTRACT/sbin/nfs-cachefs-probe" /usr/sbin/nfs-cachefs-probe
    info "binaries → /usr/sbin/{nfs-cachefs,nfs-cachefs-probe}"

    install -d -m 0755 /usr/share/man/man8
    install -m 0644 "$EXTRACT/share/man/man8/nfs-cachefs.8" /usr/share/man/man8/
    info "man page → /usr/share/man/man8/nfs-cachefs.8"

    install -d -m 0755 /etc/nfs-cachefs
    if [[ -f /etc/nfs-cachefs/daemon.toml ]]; then
        local backup
        backup=/etc/nfs-cachefs/daemon.toml.bak.$(date +%Y%m%d-%H%M%S)
        cp /etc/nfs-cachefs/daemon.toml "$backup"
        warn "existing daemon.toml backed up → $backup"
    fi
    sed -E "s|^[[:space:]]*cache_dir[[:space:]]*=.*|cache_dir = \"$CACHE_DIR\"|" \
        "$EXTRACT/etc/nfs-cachefs/daemon.toml" >/etc/nfs-cachefs/daemon.toml
    chmod 0644 /etc/nfs-cachefs/daemon.toml
    info "config  → /etc/nfs-cachefs/daemon.toml (cache_dir=$CACHE_DIR)"

    install -d -m 0755 /lib/systemd/system
    install -m 0644 "$EXTRACT/lib/systemd/system/nfs-cachefs.service" \
        /lib/systemd/system/nfs-cachefs.service
    info "unit    → /lib/systemd/system/nfs-cachefs.service"

    install -d -m 0755 /etc/systemd/system/nfs-cachefs.service.d
    cat >/etc/systemd/system/nfs-cachefs.service.d/local.conf <<EOF
# Generated by nfs-cachefs install.sh; safe to edit.
# Overrides the unit so the daemon's writable path matches your cache_dir
# and the unit waits for the cache fs to be mounted.
[Unit]
RequiresMountsFor=$CACHE_DIR

[Service]
ReadWritePaths=
ReadWritePaths=$CACHE_DIR
EOF
    info "drop-in → /etc/systemd/system/nfs-cachefs.service.d/local.conf"

    install -d -m 0755 /etc/modules-load.d
    echo cachefiles >/etc/modules-load.d/cachefiles.conf
    info "module load → /etc/modules-load.d/cachefiles.conf"
}

# ─── Cache dir setup ─────────────────────────────────────────────────────
setup_cache() {
    step "Cache directory setup ($CACHE_DIR)"
    install -d -m 0700 "$CACHE_DIR"

    local parent_mount parent_unit bind_opts="bind"
    parent_mount=$(stat -c %m "$(dirname "$CACHE_DIR")" 2>/dev/null || echo /)
    if [[ -n "$parent_mount" && "$parent_mount" != "/" ]]; then
        parent_unit=$(systemd-escape -p --suffix=mount -- "$parent_mount")
        bind_opts="bind,x-systemd.requires=$parent_unit"
    fi

    if mountpoint -q "$CACHE_DIR"; then
        info "$CACHE_DIR is already a mountpoint (skipping live bind)"
    else
        info "self-binding $CACHE_DIR (cachefiles requires its own mount)"
        mount --bind "$CACHE_DIR" "$CACHE_DIR"
    fi

    if fstab_has_target "$CACHE_DIR" none; then
        info "fstab already has bind entry for $CACHE_DIR (not modifying)"
    else
        fstab_append \
            "nfs-cachefs cache directory (must be its own mountpoint)" \
            "$CACHE_DIR  $CACHE_DIR  none  $bind_opts  0  0"
        info "fstab: bind entry appended"
    fi
}

# ─── NFS mount setup ─────────────────────────────────────────────────────
setup_nfs() {
    step "NFS mount setup ($MOUNT_DIR ← $NFS_ENDPOINT)"
    install -d -m 0755 "$MOUNT_DIR"

    if mountpoint -q "$MOUNT_DIR"; then
        warn "$MOUNT_DIR is already a mountpoint (likely no fsc); skipping fstab + mount"
        warn "  to enable caching at this path, manually unmount and rerun the installer"
        SKIP_NFS_MOUNT=1
    fi

    # NFSv3 with: fsc (enable cache), nosharecache (own SB so fsc isn't dropped
    # if the export is also mounted elsewhere without fsc), x-systemd.requires=
    # (daemon must be up first), nconnect=4 (parallelism), ro (defensive default).
    local nfs_opts="fsc,nosharecache,_netdev,x-systemd.requires=nfs-cachefs.service,vers=3,proto=tcp,nconnect=4,timeo=60,retrans=2,noatime,nodiratime,nolock,nocto,actimeo=60,acregmax=3600,ro"

    if fstab_has_target "$MOUNT_DIR" nfs; then
        warn "fstab already has an nfs entry at $MOUNT_DIR (not modifying)"
    else
        fstab_append \
            "nfs-cachefs cached NFS mount" \
            "$NFS_ENDPOINT  $MOUNT_DIR  nfs  $nfs_opts  0  0"
        info "fstab: NFS entry appended"
    fi
}

# ─── Daemon & mount ──────────────────────────────────────────────────────
start_service() {
    step "Starting daemon"

    if [[ -n "${NFSCACHEFS_NO_START:-}" ]]; then
        info "NFSCACHEFS_NO_START set; skipping modprobe + systemctl + mount"
        return 0
    fi

    info "modprobe cachefiles"
    modprobe cachefiles

    info "systemctl daemon-reload"
    systemctl daemon-reload

    info "enable + start nfs-cachefs"
    systemctl enable --now nfs-cachefs

    local _wait
    for _wait in 1 2 3 4 5 6 7 8 9 10; do
        if systemctl is-active --quiet nfs-cachefs; then
            info "nfs-cachefs is active"
            break
        fi
        sleep 1
    done

    if ! systemctl is-active --quiet nfs-cachefs; then
        err "nfs-cachefs failed to start"
        err "  inspect:  journalctl -u nfs-cachefs -e --no-pager"
        exit 1
    fi

    if [[ -z "${SKIP_NFS_MOUNT:-}" ]]; then
        info "mounting $MOUNT_DIR"
        if ! mount "$MOUNT_DIR"; then
            err "failed to mount $MOUNT_DIR"
            err "  check the new fstab entry and that the NFS server is reachable"
            exit 1
        fi
    fi
}

# ─── Verify ──────────────────────────────────────────────────────────────
verify() {
    step "Verification"
    if [[ -r /proc/fs/fscache/caches ]]; then
        info "fscache caches:"
        sed 's/^/    /' /proc/fs/fscache/caches
    fi
    if [[ -r /proc/fs/nfsfs/volumes ]]; then
        info "nfs volumes (last column = FSC):"
        sed 's/^/    /' /proc/fs/nfsfs/volumes
    fi

    cat <<EOF

${GREEN}${BOLD}nfs-cachefs ${RELEASE} installed.${RESET}
  daemon:     systemctl status nfs-cachefs
  logs:       journalctl -u nfs-cachefs -f
  config:     /etc/nfs-cachefs/daemon.toml
  drop-in:    /etc/systemd/system/nfs-cachefs.service.d/local.conf
  cache dir:  $CACHE_DIR
  nfs mount:  $MOUNT_DIR  ←  $NFS_ENDPOINT  (fsc,nosharecache)

Quick check (FSC=yes proves caching is wired up):
  cat /proc/fs/nfsfs/volumes
  cat /proc/fs/fscache/stats        # IO counters move on first read

To uninstall:
  systemctl disable --now nfs-cachefs
  umount $MOUNT_DIR $CACHE_DIR 2>/dev/null || true
  rm -f  /usr/sbin/nfs-cachefs /usr/sbin/nfs-cachefs-probe
  rm -f  /usr/share/man/man8/nfs-cachefs.8
  rm -rf /etc/nfs-cachefs /etc/systemd/system/nfs-cachefs.service.d
  rm -f  /lib/systemd/system/nfs-cachefs.service
  rm -f  /etc/modules-load.d/cachefiles.conf
  # then remove the two fstab entries appended above
EOF
}

main() {
    check_environment
    collect_inputs
    download_release
    install_files
    setup_cache
    setup_nfs
    start_service
    verify
}

main "$@"
