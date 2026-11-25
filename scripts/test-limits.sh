#!/usr/bin/env bash
# Measures generated rules on loopback in a throwaway user + network namespace.
# Needs no root and cannot affect host networking. Usage: scripts/test-limits.sh
set -euo pipefail
cd "$(dirname "$0")/.."
if [ "${1:-}" != --inner ]; then
    cargo build -q --example loopback_rules -p loadguard-core
    unit="app-loadguard-lg\\x2dtest-$$.scope" # escaped dash on purpose
    LG_UID=$(id -u) exec systemd-run --user --scope -q --unit="$unit" \
        unshare -Urn bash "$0" --inner
fi
ip link set lo up mtu 1500
cgroup=$(cut -d: -f3 /proc/self/cgroup)
measure() {
direction=$1
if [ "$direction" = none ]; then
    nft flush ruleset # only this namespace
else
    target/debug/examples/loopback_rules "$LG_UID" "${cgroup#/}" 500K "$direction" | nft -f - || return 1
fi
python3 - "$direction" <<'P'
import socket, sys, threading, time
DUR, CAP = 8, 500_000
def tcp():
    srv = socket.create_server(("127.0.0.1", 0))
    got = [0]
    def sink(c):
        while d := c.recv(65536): got[0] += len(d)
    def source(c):
        try:
            while True: c.sendall(b"x" * 65536)
        except OSError: pass
    c = socket.create_connection(srv.getsockname()); s, _ = srv.accept()
    c.settimeout(1)
    threading.Thread(target=source, args=(c,), daemon=True).start()
    threading.Thread(target=sink, args=(s,), daemon=True).start()
    time.sleep(DUR); n = got[0]; c.close()
    return n / DUR
def udp():
    r = socket.socket(socket.AF_INET, socket.SOCK_DGRAM); r.bind(("127.0.0.1", 0)); r.settimeout(1)
    w = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
    end = time.time() + DUR
    def send():
        while time.time() < end:
            try: w.sendto(b"x" * 1200, r.getsockname())
            except OSError: pass
    threading.Thread(target=send, daemon=True).start()
    n = 0
    try:
        while time.time() < end: n += len(r.recv(2048))
    except OSError: pass
    return n / DUR
ok = True
for name, rate in (("tcp", tcp()), ("udp", udp())):
    good = rate > 5 * CAP if sys.argv[1] == "none" else 0.2 * CAP < rate < 1.25 * CAP
    ok &= good
    print(f"{sys.argv[1]} {name}: {rate/1000:.0f} KB/s received (cap 500) {'ok' if good else 'FAIL'}")
raise SystemExit(not ok)
P
}
status=0
measure none || status=1
measure down || status=1
measure up || status=1
exit $status
