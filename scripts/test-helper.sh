#!/usr/bin/env bash
# End-to-end test of the real helper and CLI: private bus, mock polkit, and an
# isolated user + network namespace with a virtual Ethernet link. Needs no root.
set -euo pipefail
cd "$(dirname "$0")/.."
bin=$PWD/target/debug
if [ "${1:-}" != --inner ]; then
    cargo build -q --workspace --bins --examples
    tmp=$(mktemp -d)
    trap 'command rm -rf -- "$tmp"' EXIT
    LG_TMP=$tmp systemd-run --user --scope -q --unit="app-loadguard-lg\\x2de2e-$$.scope" \
        unshare -Un --map-current-user --keep-caps bash "$0" --inner
    exit
fi

pids=()
trap 'kill "${pids[@]}" 2>/dev/null || true' EXIT
fail() { echo "FAIL: $*"; exit 1; }
export XDG_CONFIG_HOME=$LG_TMP/cfg STATE_DIRECTORY=$LG_TMP/state
mkdir -p "$STATE_DIRECTORY"
lg() { "$bin/loadguard" "$@"; }

# Peer network namespace, linked by veth, holding the test server.
ip link set lo up
cat >"$LG_TMP/server.py" <<'P'
import socket, threading, time
def serve(port, send):
    srv = socket.create_server(("0.0.0.0", port))
    while True:
        c, _ = srv.accept()
        def handle(c=c):
            n = 0
            try:
                if send:
                    while True: c.sendall(b"x" * 65536)
                t = time.time()
                while d := c.recv(65536):
                    n += len(d)
                    if time.time() - t > 6:
                        open(f"{__file__}.up", "w").write(str(int(n / (time.time() - t) / 1000)))
                        break
            except OSError: pass
        threading.Thread(target=handle, daemon=True).start()
threading.Thread(target=serve, args=(9001, True), daemon=True).start()
serve(9002, False)
P
unshare -n sh -c 'until ip link show v1 >/dev/null 2>&1; do sleep .1; done
    ip addr add 10.9.9.2/24 dev v1; ip link set v1 up; exec python3 "$0"' "$LG_TMP/server.py" &
peer=$!; pids+=($peer)
sleep 0.5
ip link add v0 type veth peer name v1 netns "$peer"
ip addr add 10.9.9.1/24 dev v0; ip link set v0 up
sleep 1.5

cat >"$LG_TMP/client.py" <<'P'
import socket, sys, time
kind, dur, srv = sys.argv[1], float(sys.argv[2]), sys.argv[3]
c = socket.create_connection(("10.9.9.2", 9001 if kind == "down" else 9002)); c.settimeout(1)
end = time.time() + dur; n = 0; half = 0
while (now := time.time()) < end:
    try: n += len(c.recv(65536)) if kind == "down" else c.send(b"x" * 16384)
    except TimeoutError: pass
    if now > end - dur / 2 and not half: half = n
print(int((n - half) / (dur / 2) / 1000))  # KB/s over the second half
P
speed() { python3 "$LG_TMP/client.py" "$1" "${2:-8}" x; }
between() { [ "$2" -ge "$3" ] && [ "$2" -le "$4" ] || fail "$1: $2 KB/s not in $3..$4"; echo "ok: $1 ($2 KB/s)"; }

# Private bus.
cat >"$LG_TMP/bus.conf" <<X
<busconfig><type>system</type><listen>unix:path=$LG_TMP/bus</listen><auth>EXTERNAL</auth>
<policy context="default"><allow own="*"/><allow send_destination="*"/><allow receive_sender="*"/><allow user="*"/></policy></busconfig>
X
export DBUS_SYSTEM_BUS_ADDRESS=unix:path=$LG_TMP/bus
dbus-daemon --config-file="$LG_TMP/bus.conf" --nofork & pids+=($!)
sleep 0.5
polkit() { "$bin/examples/mock_polkit" "$1" >>"$LG_TMP/polkit.log" & pk=$!; pids+=($pk); sleep 0.5; }
start_helper() { "$bin/loadguard-helper" & hp=$!; pids+=($hp); sleep 1; }
app=lg-e2e

out=$(lg limit $app -d 500 2>&1) && fail "worked without a helper"
[[ $out == *"not installed"* ]] && echo "ok: missing helper is explained" || fail "missing helper message: $out"
polkit deny; start_helper
if lg limit $app -d 500 2>"$LG_TMP/err"; then fail "denied request succeeded"; fi
grep -q 'not authorized' "$LG_TMP/err" || fail "denial message: $(cat "$LG_TMP/err")"
nft list tables | grep -q loadguard && fail "denied request changed rules"
echo "ok: denied without authorization"
kill $pk; wait $pk 2>/dev/null || true; : >"$LG_TMP/polkit.log"; polkit allow

base=$(speed down 3); between "no limit" "$base" 5000 999999999
lg limit $app -d 500; lg limit $app -u 300
lg list | grep -E "$app .*500 KB/s .*300 KB/s" >/dev/null || fail "second limit replaced the first"
between "download, cap 500" "$(speed down)" 250 600
speed up 9 >/dev/null; sleep 1; up=$(cat "$LG_TMP/server.py.up" || echo 0)
between "upload as received, cap 300" "$up" 150 400
[ "$(grep -c asked "$LG_TMP/polkit.log")" = 1 ] && echo "ok: one password prompt for several changes" || fail "prompts: $(cat "$LG_TMP/polkit.log")"

lg clear $app
python3 "$LG_TMP/client.py" down 12 x >"$LG_TMP/live" & live=$!
sleep 3; lg limit $app -d 500 --until-closed >/dev/null; wait $live
between "limit applied to an open connection" "$(cat "$LG_TMP/live")" 200 650
[ -z "$(ls "$STATE_DIRECTORY")" ] && echo "ok: until-closed is not saved" || fail "until-closed saved"

nft flush ruleset; sleep 12
nft list tables | grep -q loadguard && echo "ok: rules restored after an external flush" || fail "not restored"

lg limit $app -d 500 >/dev/null
kill $hp; wait $hp 2>/dev/null || true; "$bin/loadguard-helper" --cleanup
nft list tables | grep -q loadguard && fail "cleanup left rules"
echo "ok: cleanup removes rules"
start_helper
nft list tables | grep -q loadguard || fail "limits not reapplied"
lg list | grep -q "$app.*always" || fail "saved limit lost"
echo "ok: saved limits reapplied on restart"
lg clear --all; nft list tables | grep -q loadguard && fail "clear left rules"
busctl --address="$DBUS_SYSTEM_BUS_ADDRESS" call io.github.keaexe.LoadGuard1 /io/github/keaexe/LoadGuard1 io.github.keaexe.LoadGuard1 SetLimit sttb "x" 5 0 false >/dev/null 2>&1 && fail "bad rate accepted"
echo "ok: invalid rates rejected by the helper"
echo ALL PASSED
