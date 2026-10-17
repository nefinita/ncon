#!/usr/bin/env bash
# Move ncon from tty1 to tty2 and hand tty1 to the Plasma login manager.
#
# usage: sudo ./scripts/move-ncon-to-tty2.sh [--dry-run]
#        sudo ./scripts/move-ncon-to-tty2.sh --revert
#        sudo ./scripts/move-ncon-to-tty2.sh --dm=sddm
#
# Forward mode:
#   tty1  plasmalogin (or sddm with --dm=sddm), enabled as display-manager
#   tty2  ncon@tty2 (enabled + started; waits until VT2 is foreground)
#
# What it changes:
#   1. stops + disables ncon@tty1 and removes its autovt@tty1 alias
#   2. stops + disables getty@tty1 (it must not fight the display manager)
#   3. stops + disables getty@tty2 and removes its autovt@tty2 alias
#   4. enables + starts ncon@tty2 (its Alias= creates autovt@tty2 -> ncon)
#   5. enables + starts the display manager (graphical.target pulls
#      display-manager.service in)
#
# Both ncon@.service and getty@.service declare `Alias=autovt@.service`, so the
# stale autovt@$vt symlinks have to go before enabling the replacement, or
# `systemctl enable` fails with "File exists".
#
# ncon@tty2 starts even while tty1 is active: it waits (100ms poll,
# interruptible by SIGTERM) until VT2 is actually foreground, like kmscon with
# --switchvt off. It does not steal the display from the greeter.
#
# Stopping ncon@tty1 (or getty@tty2) tears down whatever session runs on that
# VT, so if the script is started from one of those sessions it would be killed
# halfway. It detects that and re-executes itself as a transient system unit
# (systemd-run), which survives the session going away. The detection reads the
# cgroup and the logind session tty, not `ps -o tty`: inside ncon the shell's
# controlling terminal is a pts, never "tty1", so the old name check never
# fired and the script died after the first step.
#
# Rollback:
#   sudo ./scripts/move-ncon-to-tty2.sh --revert
#   # or manually:
#   sudo systemctl disable --now plasmalogin
#   sudo systemctl disable --now ncon@tty2
#   sudo rm /etc/systemd/system/autovt@tty2.service
#   sudo systemctl enable --now getty@tty2
#   sudo systemctl enable --now ncon@tty1
set -euo pipefail

SRC_VT=1
DST_VT=2
DM=plasmalogin
MODE=apply
DRY_RUN=0

for arg in "$@"; do
    case "$arg" in
        --revert) MODE=revert ;;
        --dry-run) DRY_RUN=1 ;;
        --dm=*) DM="${arg#--dm=}" ;;
        --inner) ;; # internal: already re-executed under systemd-run
        -h|--help)
            sed -n '2,40p' "$0" | sed 's/^# \{0,1\}//'
            exit 0
            ;;
        *)
            echo "unknown argument: $arg" >&2
            exit 2
            ;;
    esac
done

if (( ! DRY_RUN )) && [[ $EUID -ne 0 ]]; then
    echo "must run as root: sudo $0" >&2
    exit 1
fi

log() { printf '==> %s\n' "$*"; }
run() {
    if (( DRY_RUN )); then
        printf '  +'
        printf ' %q' "$@"
        printf '\n'
    else
        "$@"
    fi
}

# Remove a unit alias symlink we know we own (created by systemctl enable).
drop_alias() {
    local link="$1"
    [[ -L "$link" ]] || return 0
    if (( DRY_RUN )); then
        printf '  + rm -f %q\n' "$link"
    else
        rm -f "$link"
    fi
}

LOG=/tmp/ncon-vt-move.log

# True when this process lives in a session on one of the VTs we are about to
# take down (ncon@tty1 / getty@tty2), so it must not run inline.
needs_detach() {
    # (a) a child of the ncon service itself
    if grep -qE 'ncon@tty[0-9]+\.service' /proc/self/cgroup 2>/dev/null; then
        return 0
    fi
    # (b) a logind session whose VT is one we manage
    local scope sid tty
    scope="$(grep -oE 'session-[0-9]+\.scope' /proc/self/cgroup 2>/dev/null | head -1)"
    if [[ -n "$scope" ]]; then
        sid="${scope#session-}"; sid="${sid%.scope}"
        tty="$(loginctl show-session "$sid" -p TTY --value 2>/dev/null || true)"
        if [[ "$tty" == "tty${SRC_VT}" || "$tty" == "tty${DST_VT}" ]]; then
            return 0
        fi
    fi
    # (c) fallback: controlling terminal name
    tty="$(ps -o tty= -p $$ 2>/dev/null | tr -d ' ' || true)"
    [[ "$tty" == "tty${SRC_VT}" || "$tty" == "tty${DST_VT}" ]]
}

# Re-exec as a transient system unit so the rest survives its session ending.
# Output goes to $LOG (systemd-run's own stdio is not used: if the caller's
# session dies, a --pipe connection would break and could kill the script).
if (( ! DRY_RUN )) && [[ "${1:-}" != "--inner" ]] && needs_detach; then
    script="$(readlink -f "$0")"
    : >"$LOG"
    log "this shell is on a VT that will be torn down; re-executing detached"
    log "  log file: $LOG"
    log "  follow:   tail -f $LOG   (from another VT or SSH)"
    if command -v systemd-run >/dev/null 2>&1; then
        rc=0
        systemd-run --unit="ncon-vt-move-$$" --collect --wait \
            "$script" --inner "$@" || rc=$?
        cat "$LOG" 2>/dev/null || true
        exit "$rc"
    fi
    log "WARNING: systemd-run not found; continuing inline (may be killed)"
fi

# Detached run: capture everything in $LOG.
if [[ "${1:-}" == "--inner" ]]; then
    exec >>"$LOG" 2>&1
fi

command -v systemctl >/dev/null 2>&1 || {
    echo "systemctl not found" >&2
    exit 1
}
if ! systemctl cat "$DM.service" >/dev/null 2>&1; then
    echo "display manager unit '$DM.service' not found (try --dm=sddm)" >&2
    exit 1
fi

if [[ "$MODE" == revert ]]; then
    log "disabling and stopping $DM"
    run systemctl disable --now "$DM.service" || true

    log "removing ncon from VT${DST_VT}"
    run systemctl disable --now "ncon@tty${DST_VT}.service" || true
    drop_alias "/etc/systemd/system/autovt@tty${DST_VT}.service"

    log "restoring getty on VT${DST_VT}"
    run systemctl enable --now "getty@tty${DST_VT}.service" || true

    log "putting ncon back on VT${SRC_VT}"
    run systemctl disable "ncon@tty${SRC_VT}.service" || true
    drop_alias "/etc/systemd/system/autovt@tty${SRC_VT}.service"
    run systemctl enable "ncon@tty${SRC_VT}.service"
    run systemctl start "ncon@tty${SRC_VT}.service" || true

    run systemctl daemon-reload
    log "reverted: ncon on VT${SRC_VT}, display manager disabled"
    exit 0
fi

log "stopping ncon on VT${SRC_VT}"
run systemctl disable --now "ncon@tty${SRC_VT}.service" || true
# disable removes the alias, but do not rely on it
drop_alias "/etc/systemd/system/autovt@tty${SRC_VT}.service"

log "freeing VT${SRC_VT} from getty (the display manager will own it)"
run systemctl disable --now "getty@tty${SRC_VT}.service" || true
drop_alias "/etc/systemd/system/autovt@tty${SRC_VT}.service"

log "freeing VT${DST_VT} from getty"
run systemctl disable --now "getty@tty${DST_VT}.service" || true
drop_alias "/etc/systemd/system/autovt@tty${DST_VT}.service"

log "enabling ncon on VT${DST_VT}"
# Clean any stale instance state first; a leftover alias would make
# `systemctl enable` fail with "File exists".
run systemctl disable "ncon@tty${DST_VT}.service" || true
drop_alias "/etc/systemd/system/autovt@tty${DST_VT}.service"
run systemctl reset-failed "ncon@tty${DST_VT}.service" || true
run systemctl enable "ncon@tty${DST_VT}.service"
run systemctl start "ncon@tty${DST_VT}.service" || true

log "enabling and starting $DM"
# Replace a stale display-manager alias if it points at another DM.
dm_link=/etc/systemd/system/display-manager.service
if [[ -L "$dm_link" ]] \
    && [[ "$(readlink -f "$dm_link")" != "$(readlink -f "/usr/lib/systemd/system/$DM.service")" ]]; then
    run rm -f "$dm_link"
fi
run systemctl enable "$DM.service"
run systemctl start "$DM.service"

run systemctl daemon-reload

log "done"
log "  ncon        -> VT${DST_VT} (Ctrl+Alt+F${DST_VT})"
log "  ${DM} -> VT${SRC_VT} (Ctrl+Alt+F${SRC_VT})"
log "revert with: sudo $0 --revert"
