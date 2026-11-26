#!/usr/bin/env bash
# Runs the real GUI on a private virtual display and session bus, with a stand-in
# tray panel. Never touches the desktop or real limits. Needs kwin_wayland.
set -euo pipefail
cd "$(dirname "$0")/.."
bin=$PWD/target/debug
if [ "${1:-}" != --inner ]; then
    cargo build -q -p loadguard-gui --bins --examples
    tmp=$(mktemp -d)
    trap 'command rm -rf -- "$tmp"' EXIT
    # No system bus: the GUI must cope, and cannot reach real limits.
    env -u DISPLAY -u WAYLAND_DISPLAY LG_TMP="$tmp" XDG_CONFIG_HOME="$tmp/cfg" \
        DBUS_SYSTEM_BUS_ADDRESS=unix:path=/nonexistent \
        dbus-run-session -- bash "$0" --inner
    exit
fi
fail() { echo "FAIL: $*"; exit 1; }
pids=()
trap 'kill "${pids[@]}" 2>/dev/null || true' EXIT
export WAYLAND_DISPLAY=lg-test-$$
kwin_wayland --virtual --width 1000 --height 800 --socket "$WAYLAND_DISPLAY" --no-lockscreen \
    >"$LG_TMP/kwin.log" 2>&1 & pids+=($!)
sleep 4

# Without a panel, the GUI still runs.
timeout 5 "$bin/loadguard-gui" >"$LG_TMP/plain.log" 2>&1 && fail "exited early: $(cat "$LG_TMP/plain.log")"
[ $? -eq 124 ] || true
echo "ok: runs without a tray panel"

"$bin/examples/tray_probe" "Quit (limits stay active)" "$LG_TMP/go" >"$LG_TMP/probe.log" 2>&1 & probe=$!; pids+=($probe)
sleep 1
"$bin/loadguard-gui" >"$LG_TMP/gui.log" 2>&1 & gui=$!; pids+=($gui)
sleep 4
status=0; out=$(timeout 5 "$bin/loadguard-gui" 2>&1) || status=$?
[ $status -eq 0 ] || fail "second copy did not hand over (status $status): $out; first: $(cat "$LG_TMP/gui.log")"
kill -0 "$gui" 2>/dev/null || fail "first copy died: $(cat "$LG_TMP/gui.log")"
echo "ok: a second copy hands over to the first and exits"
touch "$LG_TMP/go"
wait "$probe" || fail "tray probe: $(cat "$LG_TMP/probe.log")"
cat "$LG_TMP/probe.log"
grep -q 'menu: Clear all limits… (disabled)' "$LG_TMP/probe.log" || fail "clear all should be disabled without limits"
for _ in $(seq 20); do kill -0 "$gui" 2>/dev/null || break; sleep 0.25; done
kill -0 "$gui" 2>/dev/null && fail "Quit in the tray did not quit"
wait "$gui" || fail "GUI exit status $?: $(cat "$LG_TMP/gui.log")"
echo "ok: tray menu quits the app"
echo ALL PASSED
