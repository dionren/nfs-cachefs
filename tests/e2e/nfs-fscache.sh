#!/usr/bin/env bash
# End-to-end test for nfs-cachefs against a real NFS server.
#
# Usage:
#   sudo NFS_SERVER=<server-ip> NFS_EXPORT=<export-path> \
#        TEST_FILE=<path-to-large-file-on-mount> \
#        tests/e2e/nfs-fscache.sh
#
# Prerequisites: kernel cachefiles module, nfs-common, the binary built at
# target/release/nfs-cachefs, and a writable /var/cache/fscache that is its
# own filesystem.

set -euo pipefail

NFS_SERVER="${NFS_SERVER:?set NFS_SERVER}"
NFS_EXPORT="${NFS_EXPORT:?set NFS_EXPORT}"
NFS_MOUNT="${NFS_MOUNT:-/mnt/nfs-test}"
TEST_FILE="${TEST_FILE:-${NFS_MOUNT}/test-bigfile}"
TEST_SIZE_MB="${TEST_SIZE_MB:-1024}"
CACHE_DIR="${CACHE_DIR:-/var/cache/fscache}"
DAEMON_BIN="${DAEMON_BIN:-./target/release/nfs-cachefs}"
DAEMON_CFG="${DAEMON_CFG:-/etc/nfs-cachefs/daemon.toml}"
CACHE_TAG="${CACHE_TAG:-}"

if [[ $EUID -ne 0 ]]; then
    echo "must run as root" >&2; exit 1
fi
[[ -x "$DAEMON_BIN" ]] || { echo "missing $DAEMON_BIN — run cargo build --release" >&2; exit 1; }
[[ -f "$DAEMON_CFG" ]] || { echo "missing $DAEMON_CFG — run packaging/install.sh" >&2; exit 1; }

if [[ -z "$CACHE_TAG" ]]; then
    CACHE_TAG=$(sed -n 's/^[[:space:]]*tag[[:space:]]*=[[:space:]]*"\([^"]*\)".*/\1/p' "$DAEMON_CFG" | head -n1)
    CACHE_TAG="${CACHE_TAG:-nfscache}"
fi

count_fsc_yes() {
    awk '$NF == "yes" { n++ } END { print n + 0 }' /proc/fs/nfsfs/volumes 2>/dev/null || echo 0
}

cleanup() {
    local rc=$?
    set +e
    [[ -n "${DAEMON_PID:-}" ]] && kill -TERM "$DAEMON_PID" 2>/dev/null && wait "$DAEMON_PID" 2>/dev/null
    mountpoint -q "$NFS_MOUNT" && umount "$NFS_MOUNT"
    exit "$rc"
}
trap cleanup EXIT INT TERM

echo "==> ensure cachefiles loaded"
modprobe cachefiles
[[ -e /dev/cachefiles ]] || { echo "no /dev/cachefiles" >&2; exit 1; }
mountpoint -q "$CACHE_DIR" || { echo "$CACHE_DIR is not its own mountpoint" >&2; exit 1; }

echo "==> start daemon"
"$DAEMON_BIN" --config "$DAEMON_CFG" &
DAEMON_PID=$!
# Wait for cache binding (up to 5s)
BOUND=0
for _ in {1..50}; do
    if ! kill -0 "$DAEMON_PID" 2>/dev/null; then
        set +e
        wait "$DAEMON_PID"
        rc=$?
        set -e
        echo "daemon exited before binding cache tag $CACHE_TAG (rc=$rc)" >&2
        exit 1
    fi
    if grep -Fq "$CACHE_TAG" /proc/fs/fscache/caches 2>/dev/null; then
        BOUND=1
        break
    fi
    sleep 0.1
done
if (( BOUND == 0 )); then
    echo "cache tag $CACHE_TAG did not appear in /proc/fs/fscache/caches" >&2
    cat /proc/fs/fscache/caches 2>/dev/null || true
    exit 1
fi

echo "==> mount NFS with fsc"
mkdir -p "$NFS_MOUNT"
FSC_YES_BEFORE=$(count_fsc_yes)
# nosharecache: required when the same export is already mounted elsewhere
# on the box without fsc — without it, the kernel coalesces NFS SBs by
# (server, export) and the new -o fsc mount silently inherits the
# existing non-fsc SB. Harmless when there's no competing mount.
mount -t nfs -o "fsc,nosharecache,timeo=60,retrans=2,nconnect=4,noatime,nodiratime,nolock,nfsvers=3,tcp,nocto,actimeo=60,acregmax=3600,async" \
    "${NFS_SERVER}:${NFS_EXPORT}" "$NFS_MOUNT"

echo "==> assert FSC=yes"
FSC_YES_AFTER=$(count_fsc_yes)
if (( FSC_YES_AFTER <= FSC_YES_BEFORE )); then
    echo "FSC not enabled on the new mount" >&2
    cat /proc/fs/nfsfs/volumes 2>/dev/null || true
    exit 1
fi

echo "==> drop caches"
echo 3 > /proc/sys/vm/drop_caches

echo "==> COLD read of ${TEST_SIZE_MB} MiB from $TEST_FILE"
COLD_OUT=$(dd if="$TEST_FILE" of=/dev/null bs=1M count="$TEST_SIZE_MB" 2>&1 | tail -1)
echo "$COLD_OUT"

# IO counters before hot read
WR_BEFORE=$(grep -E "^IO" /proc/fs/fscache/stats | sed -E 's/.*wr=([0-9]+).*/\1/')

echo "==> drop caches (force re-read from fscache)"
echo 3 > /proc/sys/vm/drop_caches

echo "==> HOT read of ${TEST_SIZE_MB} MiB"
HOT_OUT=$(dd if="$TEST_FILE" of=/dev/null bs=1M count="$TEST_SIZE_MB" 2>&1 | tail -1)
echo "$HOT_OUT"

WR_AFTER=$(grep -E "^IO" /proc/fs/fscache/stats | sed -E 's/.*wr=([0-9]+).*/\1/')
RD_AFTER=$(grep -E "^IO" /proc/fs/fscache/stats | sed -E 's/.*rd=([0-9]+).*/\1/')

echo
echo "==> results"
echo "  cold: $COLD_OUT"
echo "  hot:  $HOT_OUT"
echo "  fscache IO: rd=$RD_AFTER, wr (post-hot) - wr (post-cold) = $((WR_AFTER - WR_BEFORE))"
echo "  cache dir size: $(du -sh "$CACHE_DIR" | cut -f1)"

# Sanity: hot read should not have caused additional writes (data was already
# cached by cold read). Allow a small slop for metadata.
if (( WR_AFTER - WR_BEFORE > 16 )); then
    echo "WARN: hot read triggered $((WR_AFTER - WR_BEFORE)) extra writes; cache may not be hitting" >&2
fi

echo "==> SIGTERM the daemon"
kill -TERM "$DAEMON_PID"
wait "$DAEMON_PID" 2>/dev/null && echo "  daemon exited cleanly"
DAEMON_PID=""
echo "==> PASS"
