#!/bin/sh
set -eu
axit_target=${1:?Usage: package.sh TARGET}
case "$axit_target" in aarch64-apple-darwin|x86_64-apple-darwin|aarch64-unknown-linux-musl|x86_64-unknown-linux-musl) ;; *) printf '%s\n' 'Unsupported release target' >&2; exit 1 ;; esac
case "$axit_target" in
  *-apple-darwin)
    # Use the host's Mach-O tools; cross-target rust-objcopy may lack LLVM runtime libraries.
    CARGO_PROFILE_RELEASE_STRIP=none cargo build --release --locked --jobs 2 --target "$axit_target"
    strip -x "target/$axit_target/release/axit"
    codesign --force --sign - "target/$axit_target/release/axit"
    codesign --verify --strict "target/$axit_target/release/axit"
    ;;
  *) cargo build --release --locked --jobs 2 --target "$axit_target" ;;
esac
axit_tmp=$(mktemp -d)
trap 'rm -rf -- "$axit_tmp"' EXIT HUP INT TERM
cp "target/$axit_target/release/axit" "$axit_tmp/axit"
cp LICENSE "$axit_tmp/LICENSE"
mkdir -p dist
COPYFILE_DISABLE=1 tar -czf "dist/axit-$axit_target.tar.gz" -C "$axit_tmp" axit LICENSE
if command -v sha256sum >/dev/null 2>&1; then (cd dist && sha256sum "axit-$axit_target.tar.gz"); else (cd dist && shasum -a 256 "axit-$axit_target.tar.gz"); fi

