#!/bin/sh
set -eu
scry_target=${1:?Usage: package.sh TARGET}
case "$scry_target" in aarch64-apple-darwin|x86_64-apple-darwin|aarch64-unknown-linux-musl|x86_64-unknown-linux-musl) ;; *) printf '%s\n' 'Unsupported release target' >&2; exit 1 ;; esac
cargo build --release --locked --jobs 2 --target "$scry_target"
scry_tmp=$(mktemp -d)
trap 'rm -rf -- "$scry_tmp"' EXIT HUP INT TERM
cp "target/$scry_target/release/scry" "$scry_tmp/scry"
cp LICENSE "$scry_tmp/LICENSE"
mkdir -p dist
tar -czf "dist/scry-$scry_target.tar.gz" -C "$scry_tmp" scry LICENSE
if command -v sha256sum >/dev/null 2>&1; then (cd dist && sha256sum "scry-$scry_target.tar.gz"); else (cd dist && shasum -a 256 "scry-$scry_target.tar.gz"); fi

