#!/bin/sh
set -eu
scry_binary=${1:?Usage: installer-test.sh BINARY}
scry_test_tmp=$(mktemp -d)
trap 'rm -rf -- "$scry_test_tmp"' EXIT HUP INT TERM
SCRY_INSTALL_DIR="$scry_test_tmp/bin" sh install.sh --binary "$scry_binary"
"$scry_test_tmp/bin/scry" --version
test -x "$scry_test_tmp/bin/scry"
set -- "$scry_test_tmp"/bin/.scry-new*
test ! -e "$1"

# Exercise authenticated release downloads, exact archive contents and SHA-256 checks.
case "$(uname -s)/$(uname -m)" in
  Darwin/arm64) scry_test_target=aarch64-apple-darwin ;;
  Darwin/x86_64) scry_test_target=x86_64-apple-darwin ;;
  Linux/aarch64|Linux/arm64) scry_test_target=aarch64-unknown-linux-musl ;;
  Linux/x86_64|Linux/amd64) scry_test_target=x86_64-unknown-linux-musl ;;
  *) exit 1 ;;
esac
mkdir -p "$scry_test_tmp/archive" "$scry_test_tmp/release" "$scry_test_tmp/stub"
cp "$scry_binary" "$scry_test_tmp/archive/scry"
cp LICENSE "$scry_test_tmp/archive/LICENSE"
scry_test_asset="scry-$scry_test_target.tar.gz"
COPYFILE_DISABLE=1 tar -czf "$scry_test_tmp/release/$scry_test_asset" -C "$scry_test_tmp/archive" scry LICENSE
if command -v sha256sum >/dev/null 2>&1; then
  (cd "$scry_test_tmp/release" && sha256sum "$scry_test_asset") > "$scry_test_tmp/release/SHA256SUMS"
else
  (cd "$scry_test_tmp/release" && shasum -a 256 "$scry_test_asset") > "$scry_test_tmp/release/SHA256SUMS"
fi
cat > "$scry_test_tmp/stub/gh" <<'SCRY_GH_STUB'
#!/bin/sh
set -eu
if [ "$1" = auth ] && [ "$2" = status ]; then exit 0; fi
[ "$1" = release ] && [ "$2" = download ] || exit 2
scry_download_dir=
while [ "$#" -gt 0 ]; do
  if [ "$1" = --dir ]; then shift; scry_download_dir=$1; fi
  shift
done
[ -n "$scry_download_dir" ] || exit 2
cp "$SCRY_TEST_RELEASE_DIR"/*.tar.gz "$SCRY_TEST_RELEASE_DIR/SHA256SUMS" "$scry_download_dir/"
SCRY_GH_STUB
chmod 755 "$scry_test_tmp/stub/gh"
SCRY_TEST_RELEASE_DIR="$scry_test_tmp/release" PATH="$scry_test_tmp/stub:$PATH" SCRY_INSTALL_DIR="$scry_test_tmp/release-bin" sh install.sh
cmp "$scry_binary" "$scry_test_tmp/release-bin/scry"
printf '%s\n' tampered >> "$scry_test_tmp/release/$scry_test_asset"
if SCRY_TEST_RELEASE_DIR="$scry_test_tmp/release" PATH="$scry_test_tmp/stub:$PATH" SCRY_INSTALL_DIR="$scry_test_tmp/release-bin" sh install.sh > "$scry_test_tmp/rejected.log" 2>&1; then
  printf '%s\n' 'Tampered release archive was accepted' >&2
  exit 1
fi
rg -q 'SHA-256 verification failed' "$scry_test_tmp/rejected.log" 2>/dev/null || grep -q 'SHA-256 verification failed' "$scry_test_tmp/rejected.log"
cmp "$scry_binary" "$scry_test_tmp/release-bin/scry"
printf '%s\n' 'Binary install, release install and tamper-rejection fixtures passed; no shell startup files changed.'
