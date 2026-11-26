# LoadGuard

Limit the download and upload speed of individual Linux apps, including ones
already running. Early development.

LoadGuard finds apps through their systemd cgroups and limits them with
nftables. It requires cgroup v2, a systemd user session, nftables and polkit.

    loadguard-gui

or from a terminal:

    loadguard list
    loadguard limit firefox --down 500K --up 100K
    loadguard limit steam --down 20Mbit --always
    loadguard run --down 500K -- wget https://example.org/file
    loadguard clear firefox

Rates are in KB/s (1000 bytes) unless a unit such as `MB` or `Mbit` is given.
Limits end when the app closes. With `--always`, they are reapplied when the app
or the computer restarts. You are asked for your password once per login.

## Install (Arch)

    cd packaging/arch && makepkg -si
    sudo systemctl enable --now loadguard   # reapplies limits at boot

## Development

    cargo run --bin loadguard -- list
    cargo test
    scripts/test-limits.sh   # generated rules, in an isolated namespace
    scripts/test-helper.sh   # real helper and CLI, end to end; no root needed
    cargo run -p loadguard-gui --example screens -- DIR   # GUI checks and screenshots
