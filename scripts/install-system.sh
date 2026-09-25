#!/usr/bin/env bash
# Install ncon as the system console on tty1 and hand tty2 to a plain getty.
#
# usage: sudo ./scripts/install-system.sh [--manual] [--test-tty8] [--force-config]
#
# Modes:
#   default     ncon@tty1 runs as a systemd service (boxed console on tty1),
#               tty2 gets a plain getty.
#   --manual    No ncon service is enabled: every VT keeps its getty, and you
#               start ncon yourself from a login shell by typing `ncon`
#               (or `sudo ncon`, which then opens your own shell rather than
#               asking for a second login). Exiting that shell returns you to
#               the getty prompt.
#
# What it does:
#   1. installs target/release/ncon to /usr/local/bin/ncon
#   2. installs ncon@.service to /etc/systemd/system/
#   3. seeds /etc/ncon/config.toml (copying /etc/bcon/config.toml if present)
#   4. disables kmscon autostart on tty2..6 (and removes stale autovt aliases)
#   5. enables the console: ncon@tty1 (default) or getty@tty1..6 (--manual)
#   6. default mode: enables getty@tty2
#   7. optional --test-tty8: smoke-test the unit on the free tty8, then switch back
#
# Rollback:
#   sudo systemctl disable --now ncon@tty1 && sudo systemctl enable --now getty@tty1
#   sudo systemctl disable --now getty@tty2
#   sudo systemctl enable --now kmsconvt@tty2 kmsconvt@tty3 kmsconvt@tty4 kmsconvt@tty5 kmsconvt@tty6
#
# On-demand consoles (optional, kmscon-style):
#   ncon@.service declares `Alias=autovt@.service`, which is what
#   systemd-logind starts when a VT without a session is switched to. That
#   alias only takes effect once /etc/systemd/system/autovt@.service points at
#   ncon@.service — distributions usually point it at getty@.service, so
#   enabling ncon@$vt alone does not override it. To let logind spawn ncon on
#   every freshly activated VT:
#     sudo rm /etc/systemd/system/autovt@.service
#     sudo ln -s /usr/lib/systemd/system/ncon@.service /etc/systemd/system/autovt@.service
#     sudo systemctl daemon-reload
#   (Undo with: sudo ln -sf /usr/lib/systemd/system/getty@.service /etc/systemd/system/autovt@.service)
set -uo pipefail

REPO="$(cd "$(dirname "$0")/.." && pwd)"
BIN_SRC="$REPO/target/release/ncon"
UNIT_SRC="$REPO/ncon@.service"
TEST_TTY8=0
FORCE_CONFIG=0
MANUAL=0
for arg in "$@"; do
    case "$arg" in
        --test-tty8) TEST_TTY8=1 ;;
        --force-config) FORCE_CONFIG=1 ;;
        --manual) MANUAL=1 ;;
        -h|--help) sed -n '2,24p' "$0"; exit 0 ;;
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
USER_HOME=""
if [ -n "${SUDO_USER:-}" ] && [ "$SUDO_USER" != "root" ]; then
    USER_HOME="$(getent passwd "$SUDO_USER" | cut -d: -f6)"
fi

CANDIDATES=()
if [ -n "$USER_HOME" ]; then
    # the invoking user's own config is the most specific source
    CANDIDATES+=("$USER_HOME/.config/ncon/config.toml" "$USER_HOME/.config/bcon/config.toml")
fi
CANDIDATES+=(/etc/bcon/config.toml)

if [ -f /etc/ncon/config.toml ] && [ "$FORCE_CONFIG" -eq 0 ]; then
    echo "   /etc/ncon/config.toml exists, left untouched (--force-config to overwrite)"
else
    copied=0
    for src in "${CANDIDATES[@]}"; do
        if [ -f "$src" ]; then
            cp "$src" /etc/ncon/config.toml
            echo "   $src -> /etc/ncon/config.toml"
            copied=1
            break
        fi
    done
    if [ "$copied" -eq 0 ]; then
        /usr/local/bin/ncon --init-config=system >/dev/null && echo "   generated defaults"
    fi
fi
if ! grep -qE '^\s*ime\s*=\s*true' /etc/ncon/config.toml; then
    echo "   NOTE: ime is not enabled in /etc/ncon/config.toml — set 'ime = true' under [terminal]"
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

if [ "$MANUAL" -eq 1 ]; then
    echo "== 5/6 manual mode: getty on tty1..6, start ncon from a login shell"
    systemctl disable --now ncon@tty1 ncon@tty2 >/dev/null 2>&1 || true
    for vt in tty1 tty2 tty3 tty4 tty5 tty6; do
        systemctl enable "getty@$vt" 2>&1 | sed 's/^/   /'
    done
    echo "   log in and run:  ncon          (or 'sudo ncon' to open your own shell)"
    echo "   exiting that shell returns you to the getty prompt"
else
    echo "== 5/7 tty1 -> ncon"
    systemctl disable --now getty@tty1 >/dev/null 2>&1 || true
    systemctl enable ncon@tty1 2>&1 | sed 's/^/   /'
    echo "   (not started now — the current session may still own tty1)"
    echo "   start it manually after leaving the desktop: sudo systemctl start ncon@tty1"

    echo "== 6/7 tty2 -> getty"
    systemctl enable --now getty@tty2 2>&1 | sed 's/^/   /'
fi

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
units="ncon@tty1 getty@tty1 getty@tty2"
if [ "$MANUAL" -eq 1 ]; then
    units="getty@tty1 getty@tty2 getty@tty3 ncon@tty1 ncon@tty2"
fi
for unit in $units kmsconvt@tty2 kmsconvt@tty3 kmsconvt@tty4 kmsconvt@tty5 kmsconvt@tty6; do
    printf '   %-16s %s\n' "$unit" "$(systemctl is-enabled "$unit" 2>&1)"
done
echo
if [ "$MANUAL" -eq 1 ]; then
    echo "Manual mode: every VT boots to getty. Log in and type 'ncon'; exiting it"
    echo "returns you to the getty prompt. Reboot (or chvt to a VT) to activate."
else
    echo "ncon@tty1 starts on next boot (Ctrl+Alt+F1). tty2 is a plain getty."
    echo "Reboot to activate, or: sudo systemctl start ncon@tty1"
fi
