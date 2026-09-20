#!/usr/bin/env bash
# Install ncon as the system console on tty1 and hand tty2 to a plain getty.
#
# usage: sudo ./scripts/install-system.sh [--test-tty8]
#
# What it does:
#   1. installs target/release/ncon to /usr/local/bin/ncon
#   2. installs ncon@.service to /etc/systemd/system/
#   3. seeds /etc/ncon/config.toml (copying /etc/bcon/config.toml if present)
#   4. disables kmscon autostart on tty2..6 (and removes stale autovt aliases)
#   5. enables ncon@tty1 (does NOT start it now; the running desktop may own tty1)
#   6. enables getty@tty2
#   7. optional --test-tty8: smoke-test the unit on the free tty8, then switch back
#
# Rollback:
#   sudo systemctl disable --now ncon@tty1 && sudo systemctl enable --now getty@tty1
#   sudo systemctl disable --now getty@tty2
#   sudo systemctl enable --now kmsconvt@tty2 kmsconvt@tty3 kmsconvt@tty4 kmsconvt@tty5 kmsconvt@tty6
set -uo pipefail

REPO="$(cd "$(dirname "$0")/.." && pwd)"
BIN_SRC="$REPO/target/release/ncon"
UNIT_SRC="$REPO/ncon@.service"
TEST_TTY8=0
for arg in "$@"; do
    case "$arg" in
        --test-tty8) TEST_TTY8=1 ;;
        -h|--help) sed -n '2,20p' "$0"; exit 0 ;;
        *) echo "unknown option: $arg"; exit 1 ;;
    esac
done

if [ "$(id -u)" -ne 0 ]; then
    echo "please run with sudo: sudo $0 $*"
    exit 1
fi
if [ ! -x "$BIN_SRC" ]; then
    echo "missing $BIN_SRC — run 'cargo build --release' first"
    exit 1
fi

echo "== 1/7 install binary"
install -Dm755 "$BIN_SRC" /usr/local/bin/ncon

echo "== 2/7 install unit"
install -Dm644 "$UNIT_SRC" /etc/systemd/system/ncon@.service

echo "== 3/7 config"
mkdir -p /etc/ncon
if [ -f /etc/ncon/config.toml ]; then
    echo "   /etc/ncon/config.toml exists, left untouched"
elif [ -f /etc/bcon/config.toml ]; then
    cp /etc/bcon/config.toml /etc/ncon/config.toml
    echo "   copied /etc/bcon/config.toml -> /etc/ncon/config.toml"
else
    /usr/local/bin/ncon --init-config=system >/dev/null && echo "   generated defaults"
fi
systemctl daemon-reload

echo "== 4/7 disable kmscon autostart (tty2..6)"
for vt in tty2 tty3 tty4 tty5 tty6; do
    systemctl disable --now "kmsconvt@$vt" >/dev/null 2>&1 || true
    link="/etc/systemd/system/autovt@$vt.service"
    if [ -L "$link" ] && [ "$(readlink "$link")" = "/usr/lib/systemd/system/kmsconvt@.service" ]; then
        rm -f "$link"
        echo "   removed stale $link"
    fi
done
systemctl daemon-reload

echo "== 5/7 tty1 -> ncon"
systemctl disable --now getty@tty1 >/dev/null 2>&1 || true
systemctl enable ncon@tty1 2>&1 | sed 's/^/   /'
echo "   (not started now — the current session may still own tty1)"
echo "   start it manually after leaving the desktop: sudo systemctl start ncon@tty1"

echo "== 6/7 tty2 -> getty"
systemctl enable --now getty@tty2 2>&1 | sed 's/^/   /'

echo "== 7/7 kmscon units left enabled-check"
for vt in tty2 tty3 tty4 tty5 tty6; do
    printf '   kmsconvt@%s: %s\n' "$vt" "$(systemctl is-enabled kmsconvt@$vt 2>&1)"
done

if [ "$TEST_TTY8" -eq 1 ]; then
    echo "== smoke test on tty8 (screen switches for a few seconds)"
    PREV="$(tr -dc '0-9' < /sys/class/tty/tty0/active)"
    systemctl reset-failed ncon@tty8 >/dev/null 2>&1 || true
    systemctl start ncon@tty8
    sleep 1
    chvt 8 2>/dev/null || true
    sleep 4
    if systemctl is-active --quiet ncon@tty8; then
        echo "   ncon@tty8: active OK"
        journalctl -q -u ncon@tty8 --no-pager | tail -3 | sed 's/^/   | /'
    else
        echo "   ncon@tty8 FAILED:"
        journalctl -q -u ncon@tty8 --no-pager | tail -10 | sed 's/^/   | /'
    fi
    systemctl stop ncon@tty8 >/dev/null 2>&1 || true
    chvt "${PREV:-1}" 2>/dev/null || true
fi

echo
echo "== summary"
for unit in ncon@tty1 getty@tty1 getty@tty2 kmsconvt@tty2 kmsconvt@tty3 kmsconvt@tty4 kmsconvt@tty5 kmsconvt@tty6; do
    printf '   %-16s %s\n' "$unit" "$(systemctl is-enabled "$unit" 2>&1)"
done
echo
echo "ncon@tty1 starts on next boot (Ctrl+Alt+F1). tty2 is a plain getty."
echo "Reboot to activate, or: sudo systemctl start ncon@tty1"
