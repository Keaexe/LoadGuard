#!/bin/sh
# Builds the generic .tar.gz from release binaries.
# Usage: build-tarball.sh <binaries dir> <version> <output dir>
set -eu
umask 022
bin=$1
version=$2
out=$3
here=$(cd "$(dirname "$0")" && pwd)
root=$(dirname "$(dirname "$here")")
name=loadguard-$version-$(uname -m)
stage=$(mktemp -d)
trap 'rm -rf "$stage"' EXIT
# mktemp makes a private folder; packaged folders must be world-readable.
chmod 755 "$stage"

dir=$stage/$name
install -d "$dir/bin" "$dir/packaging" "$dir/crates/loadguard-gui/ui"
install -m755 "$bin/loadguard" "$bin/loadguard-gui" "$bin/loadguard-helper" "$dir/bin/"
for f in install-files.sh loadguard.service 80-loadguard.preset io.github.keaexe.LoadGuard1.service \
    io.github.keaexe.LoadGuard1.conf io.github.keaexe.LoadGuard.policy io.github.keaexe.LoadGuard.desktop; do
    cp -p "$root/packaging/$f" "$dir/packaging/"
done
cp "$root/crates/loadguard-gui/ui/icon.svg" "$dir/crates/loadguard-gui/ui/"
install -m755 "$here/install.sh" "$here/uninstall.sh" "$dir/"
cp "$root/LICENSE" "$root/README.md" "$dir/"
mkdir -p "$out"
tar -C "$stage" --owner=0 --group=0 -czf "$out/$name.tar.gz" "$name"
