#!/bin/sh
# build the toolchain and pack it as dist/pith-<os>-<arch>.tar.gz, the layout
# install.sh unpacks into PITH_HOME:
#
#   bin/pith                      the command
#   lib/pith_main, lib/ir_driver  the self-hosted compiler and ir driver
#   lib/libpith_runtime.a         the runtime every program links against
#   std/                          the standard library
#
# the asset name carries no version, so the latest release always serves the
# same url for a platform; the version is in the top-level directory name.
set -eu

case "$(uname -s)" in
    Linux) os=linux ;;
    Darwin) os=macos ;;
    *) echo "unsupported os: $(uname -s)" >&2; exit 1 ;;
esac
case "$(uname -m)" in
    x86_64 | amd64) arch=x86_64 ;;
    arm64 | aarch64) arch=aarch64 ;;
    *) echo "unsupported architecture: $(uname -m)" >&2; exit 1 ;;
esac

version=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -n 1)
asset="pith-$os-$arch"
stage="dist/pith-$version-$os-$arch"

cargo build --release
make self-host-ir-driver self-host

rm -rf "$stage" "dist/$asset.tar.gz" "dist/$asset.tar.gz.sha256"
mkdir -p "$stage/bin" "$stage/lib"
cp target/release/pith "$stage/bin/"
cp target/release/libpith_runtime.a self-host/pith_main self-host/ir_driver "$stage/lib/"
cp -R std "$stage/std"
cp LICENSE README.md "$stage/"

tar -C dist -czf "dist/$asset.tar.gz" "$(basename "$stage")"
(cd dist && shasum -a 256 "$asset.tar.gz" > "$asset.tar.gz.sha256")
echo "dist/$asset.tar.gz"
