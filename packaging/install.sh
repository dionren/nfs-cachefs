#!/usr/bin/env bash
# Install nfs-cachefs binary, systemd unit, and default config.
# Run as root from the project root after `cargo build --release`.

set -euo pipefail

if [[ $EUID -ne 0 ]]; then
    echo "install.sh must be run as root (use sudo)" >&2
    exit 1
fi

cd "$(dirname "$0")/.."

BIN_SRC="target/release/nfs-cachefs"
PROBE_SRC="target/release/nfs-cachefs-probe"
if [[ ! -x "$BIN_SRC" ]]; then
    echo "missing $BIN_SRC; run \`cargo build --release\` first" >&2
    exit 1
fi

PREFIX="${PREFIX:-/usr}"
SBINDIR="${SBINDIR:-${PREFIX}/sbin}"
SYSCONFDIR="${SYSCONFDIR:-/etc}"
SYSTEMD_UNIT_DIR="${SYSTEMD_UNIT_DIR:-/lib/systemd/system}"
CACHE_DIR="${CACHE_DIR:-/var/cache/fscache}"

install -d "$SBINDIR"
install -m 0755 "$BIN_SRC"   "$SBINDIR/nfs-cachefs"
install -m 0755 "$PROBE_SRC" "$SBINDIR/nfs-cachefs-probe"

install -d "$SYSCONFDIR/nfs-cachefs"
if [[ -f "$SYSCONFDIR/nfs-cachefs/daemon.toml" ]]; then
    echo "leaving existing $SYSCONFDIR/nfs-cachefs/daemon.toml in place"
    install -m 0644 packaging/etc/nfs-cachefs/daemon.toml \
        "$SYSCONFDIR/nfs-cachefs/daemon.toml.dist"
else
    install -m 0644 packaging/etc/nfs-cachefs/daemon.toml \
        "$SYSCONFDIR/nfs-cachefs/daemon.toml"
fi

install -d "$SYSTEMD_UNIT_DIR"
install -m 0644 packaging/systemd/nfs-cachefs.service \
    "$SYSTEMD_UNIT_DIR/nfs-cachefs.service"

MANDIR="${MANDIR:-${PREFIX}/share/man/man8}"
install -d "$MANDIR"
install -m 0644 packaging/share/man/man8/nfs-cachefs.8 \
    "$MANDIR/nfs-cachefs.8"

install -d "$CACHE_DIR"
chmod 0700 "$CACHE_DIR"

if command -v systemctl >/dev/null; then
    systemctl daemon-reload
fi

cat <<EOF
Installed:
  binaries: $SBINDIR/nfs-cachefs, $SBINDIR/nfs-cachefs-probe
  config:   $SYSCONFDIR/nfs-cachefs/daemon.toml
  unit:     $SYSTEMD_UNIT_DIR/nfs-cachefs.service
  cache:    $CACHE_DIR  (must be its own fs; mount NVMe here before enabling)

Next steps:
  1. Ensure $CACHE_DIR is the root of a dedicated filesystem.
  2. Edit $SYSCONFDIR/nfs-cachefs/daemon.toml to your liking.
  3. systemctl enable --now nfs-cachefs
  4. Add  fsc  to the relevant NFS mounts in /etc/fstab.
EOF
