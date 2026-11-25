# LoadGuard

Limit the download and upload speed of individual Linux apps, including ones
already running. **Early development: the CLI can plan and save limits, but not apply them yet.**

LoadGuard finds apps through their systemd cgroups and generates nftables rules
for them. It requires cgroup v2, a systemd user session and nftables.

    cargo run -- list
    cargo run -- limit firefox --down 500K --up 100K --dry-run

Rates are in KB/s (1000 bytes) unless a unit such as `MB` or `Mbit` is given.

## Development

    cargo test
    scripts/test-limits.sh   # measures real limits in an isolated namespace, no root
