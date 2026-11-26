#!/bin/sh
# Builds the .deb from release binaries. Runs on Debian or Ubuntu.
# Usage: build-deb.sh <binaries dir> <version> <output dir>
set -eu
bin=$1
version=$2
out=$3
here=$(cd "$(dirname "$0")" && pwd)
root=$(dirname "$(dirname "$here")")
arch=$(dpkg --print-architecture)
stage=$(mktemp -d)
trap 'rm -rf "$stage"' EXIT

"$root/packaging/install-files.sh" "$bin" "$stage"
install -Dm644 "$here/copyright" "$stage/usr/share/doc/loadguard/copyright"
install -d "$stage/DEBIAN"
for script in postinst prerm postrm; do
    install -m755 "$here/$script" "$stage/DEBIAN/$script"
done
size=$(du -sk --exclude=DEBIAN "$stage" | cut -f1)
sed -e "s/@VERSION@/$version/" -e "s/@ARCH@/$arch/" -e "s/@SIZE@/$size/" \
    "$here/control" >"$stage/DEBIAN/control"
mkdir -p "$out"
dpkg-deb --root-owner-group -Zxz --build "$stage" "$out/loadguard_${version}_$arch.deb"
