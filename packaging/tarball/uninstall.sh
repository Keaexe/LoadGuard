#!/bin/sh
# Removes a LoadGuard installed by install.sh: sudo /usr/lib/loadguard/uninstall.sh
set -eu
dir=/usr/lib/loadguard
if [ "$(id -u)" != 0 ]; then
    echo "Run as root: sudo $0" >&2
    exit 1
fi
if [ ! -f "$dir/installed-files" ]; then
    echo "No LoadGuard installed by install.sh was found." >&2
    exit 1
fi
# Stopping the helper removes all of its rules.
systemctl disable --now loadguard.service 2>/dev/null || true
while IFS= read -r file; do
    case $file in
        /usr/*) rm -f -- "$file" ;;
    esac
done <"$dir/installed-files"
rm -f "$dir/installed-files" "$dir/uninstall.sh"
rmdir "$dir" 2>/dev/null || true
systemctl daemon-reload
echo "LoadGuard is removed. Saved limits remain in /var/lib/loadguard; delete that folder to remove them."
