#!/usr/bin/env bash
# nfs-cachefs installer — full end-to-end install.
#
# Resolves binaries from one of three sources (auto-detected, first hit wins):
#   1. In-source build  — running packaging/install.sh from a built repo;
#                          uses target/release/* directly.
#   2. Offline tarball  — nfs-cachefs-linux-amd64.tar.gz in the same directory
#                          as this script (or NFSCACHEFS_TARBALL=/path/to/it).
#   3. Online           — fetched from GitHub releases.
#
# Online one-liner:
#   curl -fsSL https://github.com/dionren/nfs-cachefs/releases/latest/download/install.sh | sudo bash
#
# Offline (download tarball+script ahead of time, then on the host):
#   sudo ./install.sh                                # tarball next to script
#   sudo NFSCACHEFS_TARBALL=/path/to/tar.gz ./install.sh
#
# Non-interactive:
#   sudo CACHE_DIR=/mnt/nvme/nfs-cachefs \
#        MOUNT_DIR=/mnt/llm-data \
#        NFS_ENDPOINT=server:/export \
#        NFSCACHEFS_YES=1 \
#        ./install.sh
#
# Configuration env vars (all optional; NFS_ENDPOINT is required when no tty):
#   CACHE_DIR             cache backing dir   (default /mnt/nvme/nfs-cachefs)
#   MOUNT_DIR             nfs mount point     (default /mnt/llm-data)
#   NFS_ENDPOINT          server:/export      (no default)
#   NFS_RW                1=rw (default), 0=ro
#   NFS_NCONNECT          1..16               (default 4)
#   NFS_VERS              3 | 4 | 4.1 | 4.2   (default 3)
#   NFSCACHEFS_TARBALL    explicit tarball path (skip auto-detect + download)
#   NFSCACHEFS_RELEASE    pin to a release tag (default: latest)
#   NFSCACHEFS_YES        skip confirmation prompts
#   NFSCACHEFS_NO_START   skip modprobe + systemctl + mount

set -euo pipefail

# ─── Constants ────────────────────────────────────────────────────────────
REPO="dionren/nfs-cachefs"
RELEASE="${NFSCACHEFS_RELEASE:-latest}"
ASSET="nfs-cachefs-linux-amd64.tar.gz"
UPGRADE_MODE=""

DEFAULT_CACHE_DIR="/mnt/nvme/nfs-cachefs"
DEFAULT_MOUNT_DIR="/mnt/llm-data"
DEFAULT_NFS_ENDPOINT=""
DEFAULT_NFS_RW="1"
DEFAULT_NFS_NCONNECT="4"
DEFAULT_NFS_VERS="3"

# Resolve the script directory (empty when piped through bash, e.g. curl|bash).
if [[ -n "${BASH_SOURCE[0]:-}" && -f "${BASH_SOURCE[0]}" ]]; then
    SCRIPT_DIR=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
else
    SCRIPT_DIR=""
fi

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
    # ask VARNAME DEFAULT MSG — empty default = required
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

# ─── Upgrade detection ────────────────────────────────────────────────────
detect_upgrade() {
    [[ -x /usr/sbin/nfs-cachefs && -f /etc/nfs-cachefs/daemon.toml ]] || return 0
    UPGRADE_MODE=1
    info "existing installation detected — upgrade mode (configuration will be preserved)"
}

load_existing_config() {
    step "Reading existing configuration"

    CACHE_DIR=$(awk -F'"' '/^[[:space:]]*cache_dir[[:space:]]*=[[:space:]]*"/{print $2; exit}' \
                /etc/nfs-cachefs/daemon.toml)
    [[ -n "${CACHE_DIR:-}" ]] || { err "cannot read cache_dir from /etc/nfs-cachefs/daemon.toml"; exit 1; }
    info "cache_dir:    $CACHE_DIR"

    # Read NFS mount info from fstab (first nfs entry with fsc option)
    if [[ -z "${MOUNT_DIR:-}" ]]; then
        MOUNT_DIR=$(awk '$3=="nfs" && ("," $4 ",") ~ /,fsc,/{print $2; exit}' /etc/fstab 2>/dev/null || true)
    fi
    if [[ -z "${NFS_ENDPOINT:-}" ]]; then
        NFS_ENDPOINT=$(awk '$3=="nfs" && ("," $4 ",") ~ /,fsc,/{print $1; exit}' /etc/fstab 2>/dev/null || true)
    fi

    local opts=""
    if [[ -n "${MOUNT_DIR:-}" ]]; then
        opts=$(awk -v t="$MOUNT_DIR" '$3=="nfs" && $2==t{print $4; exit}' /etc/fstab 2>/dev/null || true)
    fi
    if [[ -z "${NFS_RW:-}" ]]; then
        [[ ",$opts," == *",ro,"* ]] && NFS_RW=0 || NFS_RW=1
    fi
    if [[ -z "${NFS_NCONNECT:-}" ]]; then
        NFS_NCONNECT=$DEFAULT_NFS_NCONNECT
        if [[ "$opts" =~ nconnect=([0-9]+) ]]; then NFS_NCONNECT=${BASH_REMATCH[1]}; fi
    fi
    if [[ -z "${NFS_VERS:-}" ]]; then
        NFS_VERS=$DEFAULT_NFS_VERS
        if [[ "$opts" =~ vers=([0-9.]+) ]]; then NFS_VERS=${BASH_REMATCH[1]}; fi
    fi

    info "mount_dir:    ${MOUNT_DIR:-(not found in fstab)}"
    info "nfs_endpoint: ${NFS_ENDPOINT:-(not found in fstab)}"
}

# ─── Environment check ────────────────────────────────────────────────────
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

    local kconfig=/boot/config-$kver
    if [[ -r "$kconfig" ]] && grep -q '^CONFIG_CACHEFILES_ONDEMAND=y' "$kconfig"; then
        warn "kernel built with CONFIG_CACHEFILES_ONDEMAND=y; this daemon uses traditional mode only"
    fi

    if [[ -e /dev/cachefiles ]] && command -v fuser >/dev/null 2>&1 \
        && fuser -s /dev/cachefiles 2>/dev/null; then
        local holder ours
        holder=$(fuser /dev/cachefiles 2>/dev/null | awk '{print $1}' | tr -d ' ')
        ours=$(systemctl show -p MainPID --value nfs-cachefs.service 2>/dev/null || true)
        if [[ -n "$holder" && -n "$ours" && "$holder" == "$ours" ]]; then
            info "/dev/cachefiles held by our nfs-cachefs daemon (pid $holder); will stop it for upgrade"
        else
            local cmd; cmd=$(ps -o comm= -p "$holder" 2>/dev/null || echo "?")
            err "/dev/cachefiles is held by pid $holder ($cmd)"
            err "  stop the existing holder first (e.g. systemctl stop cachefilesd)"
            exit 1
        fi
    fi

    if ! command -v mount.nfs >/dev/null 2>&1; then
        warn "nfs-common not installed; installing via apt"
        DEBIAN_FRONTEND=noninteractive apt-get update -qq
        DEBIAN_FRONTEND=noninteractive apt-get install -y -q nfs-common
    fi
    info "nfs client: ready"

    local missing=() c
    for c in tar systemctl awk grep mount findmnt mountpoint sed install stat systemd-escape; do
        command -v "$c" >/dev/null 2>&1 || missing+=("$c")
    done
    if (( ${#missing[@]} > 0 )); then
        err "missing required commands: ${missing[*]}"
        exit 1
    fi
    info "all environment checks passed"
}

# ─── Collect inputs ───────────────────────────────────────────────────────
collect_inputs() {
    step "Configuration"
    ask CACHE_DIR    "$DEFAULT_CACHE_DIR"    "Cache directory (will be its own mountpoint)"
    ask MOUNT_DIR    "$DEFAULT_MOUNT_DIR"    "Mount directory for the cached NFS"
    ask NFS_ENDPOINT "$DEFAULT_NFS_ENDPOINT" "NFS endpoint (server:/export)"
    NFS_RW="${NFS_RW:-$DEFAULT_NFS_RW}"
    NFS_NCONNECT="${NFS_NCONNECT:-$DEFAULT_NFS_NCONNECT}"
    NFS_VERS="${NFS_VERS:-$DEFAULT_NFS_VERS}"

    [[ "$CACHE_DIR" == /* ]] || { err "CACHE_DIR must be absolute"; exit 1; }
    [[ "$MOUNT_DIR" == /* ]] || { err "MOUNT_DIR must be absolute"; exit 1; }
    [[ "$NFS_ENDPOINT" == *:/* ]] || { err "NFS_ENDPOINT must be 'server:/export'"; exit 1; }
    [[ "$CACHE_DIR" != "$MOUNT_DIR" ]] || { err "CACHE_DIR and MOUNT_DIR must differ"; exit 1; }
    [[ "$NFS_RW" =~ ^[01]$ ]] || { err "NFS_RW must be 0 or 1"; exit 1; }
    [[ "$NFS_NCONNECT" =~ ^[0-9]+$ ]] && (( NFS_NCONNECT >= 1 && NFS_NCONNECT <= 16 )) \
        || { err "NFS_NCONNECT must be 1..16"; exit 1; }
    [[ "$NFS_VERS" =~ ^(3|4|4\.0|4\.1|4\.2)$ ]] || { err "NFS_VERS must be 3 or 4.x"; exit 1; }

    local rw_label="rw"
    [[ "$NFS_RW" == "0" ]] && rw_label="ro"

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
    info "options:         $rw_label, vers=$NFS_VERS, nconnect=$NFS_NCONNECT"
    confirm "Proceed with installation?" Y || { err "aborted by user"; exit 1; }
}

# ─── Source resolution ────────────────────────────────────────────────────
# Sets SRC_BIN, SRC_PROBE, SRC_CONFIG, SRC_UNIT, SRC_MANPAGE, SRC_VERSION.
resolve_source() {
    step "Locating release artifacts"

    # 1. Explicit tarball
    if [[ -n "${NFSCACHEFS_TARBALL:-}" ]]; then
        [[ -f "$NFSCACHEFS_TARBALL" ]] || { err "NFSCACHEFS_TARBALL=$NFSCACHEFS_TARBALL not found"; exit 1; }
        info "explicit tarball: $NFSCACHEFS_TARBALL"
        extract_tarball "$NFSCACHEFS_TARBALL"
        return
    fi

    # 2. In-source build (running packaging/install.sh after cargo build)
    if [[ -n "$SCRIPT_DIR" \
        && -x "$SCRIPT_DIR/../target/release/nfs-cachefs" \
        && -f "$SCRIPT_DIR/etc/nfs-cachefs/daemon.toml" ]]; then
        local repo_root; repo_root=$(cd "$SCRIPT_DIR/.." && pwd)
        info "in-source build: $repo_root"
        SRC_BIN="$repo_root/target/release/nfs-cachefs"
        SRC_PROBE="$repo_root/target/release/nfs-cachefs-probe"
        SRC_CONFIG="$SCRIPT_DIR/etc/nfs-cachefs/daemon.toml"
        SRC_UNIT="$SCRIPT_DIR/systemd/nfs-cachefs.service"
        SRC_MANPAGE="$SCRIPT_DIR/share/man/man8/nfs-cachefs.8"
        SRC_VERSION=unknown
        if [[ -f "$repo_root/Cargo.toml" ]]; then
            SRC_VERSION=$(awk -F'"' '/^version[[:space:]]*=/ {print $2; exit}' "$repo_root/Cargo.toml")
            SRC_VERSION=${SRC_VERSION:-unknown}
        fi
        info "version: $SRC_VERSION"
        return
    fi

    # 3. Offline tarball next to script
    if [[ -n "$SCRIPT_DIR" && -f "$SCRIPT_DIR/$ASSET" ]]; then
        info "offline tarball: $SCRIPT_DIR/$ASSET"
        extract_tarball "$SCRIPT_DIR/$ASSET"
        return
    fi

    # 4. Online — download from GitHub
    download_release
}

# Extract tarball into $WORKDIR/nfs-cachefs and populate SRC_* vars.
extract_tarball() {
    local tarball=$1
    WORKDIR=$(mktemp -d /tmp/nfs-cachefs-install.XXXXXX)

    if [[ -f "$tarball.sha256" ]]; then
        (cd "$(dirname "$tarball")" && sha256sum -c "$(basename "$tarball.sha256")" >/dev/null) \
            || { err "checksum mismatch on $tarball"; exit 1; }
        info "checksum verified"
    fi

    info "extracting $tarball"
    tar xzf "$tarball" -C "$WORKDIR"
    local extract="$WORKDIR/nfs-cachefs"
    [[ -x "$extract/sbin/nfs-cachefs" ]] || { err "tarball missing sbin/nfs-cachefs"; exit 1; }
    SRC_BIN="$extract/sbin/nfs-cachefs"
    SRC_PROBE="$extract/sbin/nfs-cachefs-probe"
    SRC_CONFIG="$extract/etc/nfs-cachefs/daemon.toml"
    SRC_UNIT="$extract/lib/systemd/system/nfs-cachefs.service"
    SRC_MANPAGE="$extract/share/man/man8/nfs-cachefs.8"
    SRC_VERSION=$([[ -f "$extract/VERSION" ]] && cat "$extract/VERSION" || echo unknown)
    info "version: $SRC_VERSION"
}

download_release() {
    if ! command -v curl >/dev/null 2>&1; then
        err "curl not found and no offline tarball available"
        err "  options: install curl, drop $ASSET next to this script,"
        err "  or set NFSCACHEFS_TARBALL=/path/to/$ASSET"
        exit 1
    fi
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
        (cd "$WORKDIR" && sha256sum -c "$ASSET.sha256" >/dev/null) \
            || { err "checksum mismatch"; exit 1; }
        info "checksum verified"
    else
        warn "no checksum file available; skipping verification"
    fi

    info "extracting tarball"
    tar xzf "$WORKDIR/$ASSET" -C "$WORKDIR"
    local extract="$WORKDIR/nfs-cachefs"
    [[ -x "$extract/sbin/nfs-cachefs" ]] || { err "tarball missing sbin/nfs-cachefs"; exit 1; }
    SRC_BIN="$extract/sbin/nfs-cachefs"
    SRC_PROBE="$extract/sbin/nfs-cachefs-probe"
    SRC_CONFIG="$extract/etc/nfs-cachefs/daemon.toml"
    SRC_UNIT="$extract/lib/systemd/system/nfs-cachefs.service"
    SRC_MANPAGE="$extract/share/man/man8/nfs-cachefs.8"
    SRC_VERSION=$([[ -f "$extract/VERSION" ]] && cat "$extract/VERSION" || echo unknown)
    info "version: $SRC_VERSION"
}

# ─── fstab helpers ────────────────────────────────────────────────────────
fstab_backup_once() {
    [[ -n "${FSTAB_BACKED_UP:-}" ]] && return 0
    local stamp; stamp=$(date +%Y%m%d-%H%M%S)
    local backup=/etc/fstab.bak.$stamp
    cp -a /etc/fstab "$backup"
    FSTAB_BACKED_UP=1
    info "fstab backed up → $backup"
}

# Find the options field of an active fstab entry at TARGET with FSTYPE.
fstab_options_at() {
    awk -v t="$1" -v ft="$2" '
        $0 ~ /^[[:space:]]*#/ {next}
        NF >= 4 && $2 == t && $3 == ft { print $4; exit }
    ' /etc/fstab
}

# Append a new fstab line if no entry exists at TARGET with FSTYPE. If
# one exists, replace it when the existing options either MISS any of
# REQUIRED_OPTS or CONTAIN any of FORBIDDEN_OPTS. Each opt is matched as
# a comma-separated token, either a bare flag (e.g. "fsc") or a
# fully-qualified key=value (e.g. "x-systemd.requires=foo.service").
fstab_upsert() {
    local target=$1 fstype=$2 required=$3 forbidden=$4 line=$5 comment=$6
    local cur missing present tmp
    cur=$(fstab_options_at "$target" "$fstype")

    if [[ -z "$cur" ]]; then
        fstab_backup_once
        printf '\n# %s\n%s\n' "$comment" "$line" >> /etc/fstab
        info "fstab: appended new entry for $target"
        return
    fi

    missing=""
    present=""
    local IFS=,
    local k opt found

    for k in $required; do
        found=0
        for opt in $cur; do
            if [[ "$opt" == "$k" || "$opt" == "$k="* ]]; then
                found=1
                break
            fi
        done
        (( found )) || missing+=" $k"
    done

    if [[ -n "$forbidden" ]]; then
        for k in $forbidden; do
            for opt in $cur; do
                # Forbidden entries can be exact (option=value) or a bare flag.
                if [[ "$opt" == "$k" ]]; then
                    present+=" $k"
                    break
                fi
            done
        done
    fi

    if [[ -z "$missing" && -z "$present" ]]; then
        info "fstab: existing entry at $target already canonical; not modifying"
        return
    fi

    fstab_backup_once
    tmp=$(mktemp)
    awk -v t="$target" -v ft="$fstype" -v repl="$line" -v cmt="$comment" '
        BEGIN { done = 0 }
        /^[[:space:]]*#/ { print; next }
        {
            if (!done && NF >= 3 && $2 == t && $3 == ft) {
                print "# replaced " strftime("%F %T") " — " cmt
                print "# " $0
                print repl
                done = 1
                next
            }
            print
        }
    ' /etc/fstab > "$tmp"
    install -m 0644 "$tmp" /etc/fstab
    rm -f "$tmp"
    local why=""
    [[ -n "$missing" ]] && why+="missing:$missing "
    [[ -n "$present" ]] && why+="deprecated:$present"
    info "fstab: replaced existing entry at $target ($why)"
}

# ─── Install files ────────────────────────────────────────────────────────
install_files() {
    step "Installing files (version $SRC_VERSION)"

    # We deliberately do NOT stop the running daemon here. Replacing the
    # binary on top of a running process is safe (kernel keeps the old text
    # mmap'd until exit), and start_service will restart the daemon at the
    # end. Stopping mid-install would also cascade-unmount any NFS targets
    # that listed the service as a dependency, leaving stale superblocks
    # behind when the kernel re-registers fscache cookies on remount.

    install -m 0755 "$SRC_BIN"   /usr/sbin/nfs-cachefs
    install -m 0755 "$SRC_PROBE" /usr/sbin/nfs-cachefs-probe
    info "binaries → /usr/sbin/{nfs-cachefs,nfs-cachefs-probe}"

    install -d -m 0755 /usr/share/man/man8
    install -m 0644 "$SRC_MANPAGE" /usr/share/man/man8/nfs-cachefs.8
    info "man page → /usr/share/man/man8/nfs-cachefs.8"

    install -d -m 0755 /etc/nfs-cachefs
    if [[ -f /etc/nfs-cachefs/daemon.toml ]]; then
        # Preserve user customizations; only retarget cache_dir in place.
        local stamp; stamp=$(date +%Y%m%d-%H%M%S)
        local backup=/etc/nfs-cachefs/daemon.toml.bak.$stamp
        cp /etc/nfs-cachefs/daemon.toml "$backup"
        sed -E -i "s|^[[:space:]]*cache_dir[[:space:]]*=.*|cache_dir = \"$CACHE_DIR\"|" \
            /etc/nfs-cachefs/daemon.toml
        info "config  → /etc/nfs-cachefs/daemon.toml (preserved; cache_dir=$CACHE_DIR; backup=$backup)"
        # Also drop the shipped template alongside for reference.
        install -m 0644 "$SRC_CONFIG" /etc/nfs-cachefs/daemon.toml.dist
    else
        sed -E "s|^[[:space:]]*cache_dir[[:space:]]*=.*|cache_dir = \"$CACHE_DIR\"|" \
            "$SRC_CONFIG" >/etc/nfs-cachefs/daemon.toml
        chmod 0644 /etc/nfs-cachefs/daemon.toml
        info "config  → /etc/nfs-cachefs/daemon.toml (cache_dir=$CACHE_DIR)"
    fi

    install -d -m 0755 /lib/systemd/system
    install -m 0644 "$SRC_UNIT" /lib/systemd/system/nfs-cachefs.service
    info "unit    → /lib/systemd/system/nfs-cachefs.service"

    install -d -m 0755 /etc/systemd/system/nfs-cachefs.service.d
    cat >/etc/systemd/system/nfs-cachefs.service.d/local.conf <<EOF
# Generated by nfs-cachefs install.sh; safe to hand-edit.
# Overrides the unit so the daemon's writable path tracks cache_dir and the
# unit waits for the cache filesystem to be mounted.
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

# ─── Cache dir setup ──────────────────────────────────────────────────────
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

    fstab_upsert "$CACHE_DIR" none "bind" "" \
        "$CACHE_DIR  $CACHE_DIR  none  $bind_opts  0  0" \
        "nfs-cachefs cache directory (must be its own mountpoint)"
}

# ─── NFS mount setup ──────────────────────────────────────────────────────
setup_nfs() {
    step "NFS mount setup ($MOUNT_DIR ← $NFS_ENDPOINT)"
    install -d -m 0755 "$MOUNT_DIR"

    # We rely on the daemon's `Before=remote-fs-pre.target` ordering for
    # boot-time correctness, NOT on x-systemd.requires=. The latter would
    # cascade-unmount on every daemon restart (and on graceful upgrades),
    # producing stale fscache superblocks when remount races the kernel's
    # cookie cleanup.
    local rw_opt="rw"; [[ "$NFS_RW" == "0" ]] && rw_opt="ro"
    local nfs_opts="auto,_netdev,fsc,nosharecache,vers=$NFS_VERS,proto=tcp,nconnect=$NFS_NCONNECT,timeo=60,retrans=2,noatime,nodiratime,nolock,nocto,actimeo=60,acregmax=3600,$rw_opt"
    local nfs_line="$NFS_ENDPOINT  $MOUNT_DIR  nfs  $nfs_opts  0  0"

    # If MOUNT_DIR is currently mounted without fsc, offer to unmount so the
    # new fsc mount can take over once fstab is rewritten. start_service
    # will (re)mount it after the daemon is up.
    if mountpoint -q "$MOUNT_DIR"; then
        local cur; cur=$(findmnt -no OPTIONS "$MOUNT_DIR" 2>/dev/null || true)
        if [[ ",$cur," == *",fsc,"* ]]; then
            info "$MOUNT_DIR already mounted with fsc; leaving live mount alone"
        else
            warn "$MOUNT_DIR is currently mounted WITHOUT fsc:"
            warn "  $cur"
            if confirm "Unmount it now so the new fsc mount can replace it?" Y; then
                if command -v fuser >/dev/null 2>&1; then
                    local users
                    users=$(fuser -m "$MOUNT_DIR" 2>/dev/null \
                            | tr -s '[:space:]' ' ' | sed 's/^ *//;s/ *$//')
                    if [[ -n "$users" ]]; then
                        err "$MOUNT_DIR has active users (pids: $users)"
                        fuser -mv "$MOUNT_DIR" >&2 || true
                        exit 1
                    fi
                fi
                umount "$MOUNT_DIR" || { err "umount $MOUNT_DIR failed"; exit 1; }
                info "$MOUNT_DIR unmounted; will remount with fsc after daemon starts"
            else
                warn "leaving $MOUNT_DIR mounted without fsc; fstab updated for next boot"
            fi
        fi
    fi

    fstab_upsert "$MOUNT_DIR" nfs \
        "fsc,nosharecache" \
        "x-systemd.requires=nfs-cachefs.service" \
        "$nfs_line" \
        "nfs-cachefs cached NFS mount ($NFS_ENDPOINT)"
}

# ─── Daemon & mount ───────────────────────────────────────────────────────
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

    if systemctl is-active --quiet nfs-cachefs 2>/dev/null; then
        info "restarting nfs-cachefs to pick up the new binary + unit"
        systemctl restart nfs-cachefs
    else
        info "enable + start nfs-cachefs"
        systemctl enable --now nfs-cachefs
    fi

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

    if [[ -n "${MOUNT_DIR:-}" ]]; then
        if mountpoint -q "$MOUNT_DIR"; then
            info "$MOUNT_DIR already mounted; not remounting"
        else
            info "mounting $MOUNT_DIR"
            if ! mount "$MOUNT_DIR"; then
                err "failed to mount $MOUNT_DIR"
                err "  check the new fstab entry and that the NFS server is reachable"
                exit 1
            fi
        fi
    fi
}

# ─── Verify ───────────────────────────────────────────────────────────────
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

    local rw_label="rw"; [[ "${NFS_RW:-1}" == "0" ]] && rw_label="ro"
    cat <<EOF

${GREEN}${BOLD}nfs-cachefs $SRC_VERSION installed.${RESET}
  daemon:     systemctl status nfs-cachefs
  logs:       journalctl -u nfs-cachefs -f
  config:     /etc/nfs-cachefs/daemon.toml
  drop-in:    /etc/systemd/system/nfs-cachefs.service.d/local.conf
  cache dir:  $CACHE_DIR
  nfs mount:  ${MOUNT_DIR:-(see /etc/fstab)}  ←  ${NFS_ENDPOINT:-(see /etc/fstab)}  ($rw_label, fsc, vers=${NFS_VERS:-?}, nconnect=${NFS_NCONNECT:-?})

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
    detect_upgrade
    check_environment
    if [[ -n "$UPGRADE_MODE" ]]; then
        load_existing_config
        resolve_source
        install_files
        start_service
        verify
    else
        collect_inputs
        resolve_source
        install_files
        setup_cache
        setup_nfs
        start_service
        verify
    fi
}

main "$@"
