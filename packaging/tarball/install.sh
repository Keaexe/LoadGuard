#!/bin/sh
# Installs LoadGuard for every user. Run from the unpacked folder: sudo ./install.sh
set -eu
umask 022
here=$(cd "$(dirname "$0")" && pwd)
dir=/usr/lib/loadguard
# For tests only: installs below this folder and leaves systemd alone.
dest=${DESTDIR:-}

if [ -z "$dest" ] && [ "$(id -u)" != 0 ]; then
    echo "Run as root: sudo $0" >&2
    exit 1
fi
# Never overwrite files that a package manager installed.
[ -n "$dest" ] || for check in "pacman -Qqo" "dpkg -S" "rpm -qf"; do
    tool=${check%% *}
    if command -v "$tool" >/dev/null && [ -e /usr/bin/loadguard ] \
        && $check /usr/bin/loadguard >/dev/null 2>&1; then
        echo "LoadGuard is already installed as a package; remove that first." >&2
        exit 1
    fi
done
missing=
command -v nft >/dev/null || [ -x /usr/sbin/nft ] || missing="$missing nftables"
[ -d /usr/share/polkit-1 ] || missing="$missing polkit"
[ -d /run/systemd/system ] || missing="$missing systemd"
if [ -z "$dest" ] && [ -n "$missing" ]; then
    echo "Missing:$missing. Install them with your package manager first." >&2
    exit 1
fi

stage=$(mktemp -d)
trap 'rm -rf "$stage"' EXIT
"$here/packaging/install-files.sh" "$here/bin" "$stage"
# File by file: copying the whole tree would also overwrite the permissions of
# existing folders, / and /usr included.
(cd "$stage" && find . -type f) | while IFS= read -r file; do
    install -D -m "$(stat -c %a "$stage/$file")" "$stage/$file" "$dest/${file#./}"
done
# Remember what was installed, so that uninstall.sh removes exactly that.
(cd "$stage" && find . -type f | sed 's|^\.||') >"$dest$dir/installed-files"
install -m755 "$here/uninstall.sh" "$dest$dir/uninstall.sh"
if [ -n "$dest" ]; then
    exit 0
fi

systemctl daemon-reload
busctl call org.freedesktop.DBus /org/freedesktop/DBus org.freedesktop.DBus ReloadConfig >/dev/null 2>&1 || true
systemctl enable loadguard.service
systemctl restart loadguard.service
echo "LoadGuard is installed. Open it from your app menu, or run loadguard-gui."
echo "To remove it: sudo $dir/uninstall.sh"
