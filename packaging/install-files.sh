#!/bin/sh
# Copies LoadGuard into a directory tree, as every package lays it out.
# Usage: install-files.sh <binaries dir> <destination root> [helper dir]
# The helper dir defaults to /usr/lib/loadguard; Fedora wants /usr/libexec/loadguard.
set -eu
bin=$1
dest=$2
helper_dir=${3:-/usr/lib/loadguard}
here=$(cd "$(dirname "$0")" && pwd)
root=$(dirname "$here")
id=io.github.keaexe.LoadGuard

install -Dm755 "$bin/loadguard" "$dest/usr/bin/loadguard"
install -Dm755 "$bin/loadguard-gui" "$dest/usr/bin/loadguard-gui"
install -Dm755 "$bin/loadguard-helper" "$dest$helper_dir/loadguard-helper"
install -d "$dest/usr/lib/systemd/system"
sed "s|/usr/lib/loadguard/loadguard-helper|$helper_dir/loadguard-helper|g" \
    "$here/loadguard.service" >"$dest/usr/lib/systemd/system/loadguard.service"
chmod 644 "$dest/usr/lib/systemd/system/loadguard.service"
# Reapplies saved limits at boot on distributions that follow presets.
install -Dm644 "$here/80-loadguard.preset" "$dest/usr/lib/systemd/system-preset/80-loadguard.preset"
install -Dm644 "$here/${id}1.service" "$dest/usr/share/dbus-1/system-services/${id}1.service"
install -Dm644 "$here/${id}1.conf" "$dest/usr/share/dbus-1/system.d/${id}1.conf"
install -Dm644 "$here/$id.policy" "$dest/usr/share/polkit-1/actions/$id.policy"
install -Dm644 "$here/$id.desktop" "$dest/usr/share/applications/$id.desktop"
install -Dm644 "$root/crates/loadguard-gui/ui/icon.svg" "$dest/usr/share/icons/hicolor/scalable/apps/$id.svg"
