#!/bin/sh
set -eu
axit_binary=${1:?Usage: installer-test.sh BINARY}
axit_test_tmp=$(mktemp -d)
trap 'rm -rf -- "$axit_test_tmp"' EXIT HUP INT TERM
AXIT_INSTALL_DIR="$axit_test_tmp/bin" sh install.sh --binary "$axit_binary"
"$axit_test_tmp/bin/axit" --version
test -x "$axit_test_tmp/bin/axit"
set -- "$axit_test_tmp"/bin/.axit-new*
test ! -e "$1"

# Exercise authenticated release downloads, exact archive contents and SHA-256 checks.
case "$(uname -s)/$(uname -m)" in
  Darwin/arm64) axit_test_target=aarch64-apple-darwin ;;
  Darwin/x86_64) axit_test_target=x86_64-apple-darwin ;;
  Linux/aarch64|Linux/arm64) axit_test_target=aarch64-unknown-linux-musl ;;
  Linux/x86_64|Linux/amd64) axit_test_target=x86_64-unknown-linux-musl ;;
  *) exit 1 ;;
esac
mkdir -p "$axit_test_tmp/archive" "$axit_test_tmp/release" "$axit_test_tmp/stub"
cp "$axit_binary" "$axit_test_tmp/archive/axit"
cp LICENSE "$axit_test_tmp/archive/LICENSE"
axit_test_asset="axit-$axit_test_target.tar.gz"
COPYFILE_DISABLE=1 tar -czf "$axit_test_tmp/release/$axit_test_asset" -C "$axit_test_tmp/archive" axit LICENSE
if command -v sha256sum >/dev/null 2>&1; then
  (cd "$axit_test_tmp/release" && sha256sum "$axit_test_asset") > "$axit_test_tmp/release/SHA256SUMS"
else
  (cd "$axit_test_tmp/release" && shasum -a 256 "$axit_test_asset") > "$axit_test_tmp/release/SHA256SUMS"
fi
cat > "$axit_test_tmp/stub/gh" <<'AXIT_GH_STUB'
#!/bin/sh
set -eu
if [ "$1" = auth ] && [ "$2" = status ]; then exit 0; fi
[ "$1" = release ] && [ "$2" = download ] || exit 2
axit_download_dir=
while [ "$#" -gt 0 ]; do
  if [ "$1" = --dir ]; then shift; axit_download_dir=$1; fi
  shift
done
[ -n "$axit_download_dir" ] || exit 2
cp "$AXIT_TEST_RELEASE_DIR"/*.tar.gz "$AXIT_TEST_RELEASE_DIR/SHA256SUMS" "$axit_download_dir/"
AXIT_GH_STUB
chmod 755 "$axit_test_tmp/stub/gh"
AXIT_TEST_RELEASE_DIR="$axit_test_tmp/release" PATH="$axit_test_tmp/stub:$PATH" AXIT_INSTALL_DIR="$axit_test_tmp/release-bin" sh install.sh
cmp "$axit_binary" "$axit_test_tmp/release-bin/axit"
printf '%s\n' tampered >> "$axit_test_tmp/release/$axit_test_asset"
if AXIT_TEST_RELEASE_DIR="$axit_test_tmp/release" PATH="$axit_test_tmp/stub:$PATH" AXIT_INSTALL_DIR="$axit_test_tmp/release-bin" sh install.sh > "$axit_test_tmp/rejected.log" 2>&1; then
  printf '%s\n' 'Tampered release archive was accepted' >&2
  exit 1
fi
rg -q 'SHA-256 verification failed' "$axit_test_tmp/rejected.log" 2>/dev/null || grep -q 'SHA-256 verification failed' "$axit_test_tmp/rejected.log"
cmp "$axit_binary" "$axit_test_tmp/release-bin/axit"
printf '%s\n' 'Binary install, release install and tamper-rejection fixtures passed; no shell startup files changed.'
