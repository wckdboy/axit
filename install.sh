#!/bin/sh
set -eu

# Local source installs require Rust; release installs need only curl, tar and SHA-256.
axit_repo=${AXIT_REPO:-${SCRY_REPO:-wckdboy/axit}}
axit_version=${AXIT_VERSION:-${SCRY_VERSION:-latest}}
axit_install=${AXIT_INSTALL_DIR:-${SCRY_INSTALL_DIR:-"$HOME/.local/bin"}}
axit_source=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
axit_mode=${1:-release}
axit_tmp=$(mktemp -d "${TMPDIR:-/tmp}/axit-install.XXXXXXXX")
axit_staged=
axit_cleanup() {
  rm -rf -- "$axit_tmp"
  if [ -n "$axit_staged" ]; then rm -f -- "$axit_staged"; fi
}
trap axit_cleanup EXIT HUP INT TERM

case "$(uname -s)/$(uname -m)" in
  Darwin/arm64) axit_target=aarch64-apple-darwin ;;
  Darwin/x86_64) axit_target=x86_64-apple-darwin ;;
  Linux/aarch64|Linux/arm64) axit_target=aarch64-unknown-linux-musl ;;
  Linux/x86_64|Linux/amd64) axit_target=x86_64-unknown-linux-musl ;;
  *) printf '%s\n' 'AXIT supports macOS/Linux on ARM64 and x86-64.' >&2; exit 1 ;;
esac

if [ "$axit_mode" = --local ]; then
  [ -f "$axit_source/Cargo.toml" ] || { printf '%s\n' 'Run --local from the AXIT source checkout.' >&2; exit 1; }
  command -v cargo >/dev/null 2>&1 || { printf '%s\n' 'Source installation requires Rust 1.88+. Release packages do not require Rust.' >&2; exit 1; }
  (cd "$axit_source" && cargo build --release --locked --jobs 2)
  cp -- "$axit_source/target/release/axit" "$axit_tmp/axit"
elif [ "$axit_mode" = --binary ]; then
  [ "$#" -eq 2 ] || { printf '%s\n' 'Usage: install.sh --binary /path/to/axit' >&2; exit 1; }
  cp -- "$2" "$axit_tmp/axit"
else
  [ "$axit_mode" = release ] || { printf '%s\n' 'Usage: install.sh [--local | --binary PATH]' >&2; exit 1; }
  axit_asset="axit-$axit_target.tar.gz"
  if command -v gh >/dev/null 2>&1 && gh auth status >/dev/null 2>&1; then
    if [ "$axit_version" = latest ]; then
      gh release download --repo "$axit_repo" --pattern "$axit_asset" --pattern SHA256SUMS --dir "$axit_tmp"
    else
      gh release download "$axit_version" --repo "$axit_repo" --pattern "$axit_asset" --pattern SHA256SUMS --dir "$axit_tmp"
    fi
  else
    command -v curl >/dev/null 2>&1 || { printf '%s\n' 'curl is required for release downloads.' >&2; exit 1; }
    if [ "$axit_version" = latest ]; then axit_base="https://github.com/$axit_repo/releases/latest/download"; else axit_base="https://github.com/$axit_repo/releases/download/$axit_version"; fi
    curl --fail --silent --show-error --location --proto '=https' "$axit_base/$axit_asset" --output "$axit_tmp/$axit_asset"
    curl --fail --silent --show-error --location --proto '=https' "$axit_base/SHA256SUMS" --output "$axit_tmp/SHA256SUMS"
  fi
  axit_expected=$(awk -v name="$axit_asset" '$2 == name {print $1}' "$axit_tmp/SHA256SUMS")
  [ "${#axit_expected}" -eq 64 ] || { printf '%s\n' 'Missing or ambiguous SHA-256 checksum.' >&2; exit 1; }
  if command -v sha256sum >/dev/null 2>&1; then axit_actual=$(sha256sum "$axit_tmp/$axit_asset" | awk '{print $1}');
  else axit_actual=$(shasum -a 256 "$axit_tmp/$axit_asset" | awk '{print $1}'); fi
  [ "$axit_actual" = "$axit_expected" ] || { printf '%s\n' 'SHA-256 verification failed; nothing installed.' >&2; exit 1; }
  axit_members=$(tar -tzf "$axit_tmp/$axit_asset")
  [ "$axit_members" = "axit
LICENSE" ] || { printf '%s\n' 'Unexpected archive contents; nothing installed.' >&2; exit 1; }
  tar -xzf "$axit_tmp/$axit_asset" -C "$axit_tmp"
  [ -f "$axit_tmp/axit" ] && [ ! -L "$axit_tmp/axit" ] || { printf '%s\n' 'Invalid binary package.' >&2; exit 1; }
fi

chmod 755 "$axit_tmp/axit"
"$axit_tmp/axit" --version
mkdir -p -- "$axit_install"
[ ! -L "$axit_install/axit" ] || { printf '%s\n' 'Existing axit is a symlink; choose AXIT_INSTALL_DIR explicitly.' >&2; exit 1; }
[ ! -e "$axit_install/axit" ] || [ -f "$axit_install/axit" ] || { printf '%s\n' 'Existing axit is not a regular file; nothing installed.' >&2; exit 1; }
axit_staged=$(mktemp "$axit_install/.axit-new.XXXXXXXX")
cp -- "$axit_tmp/axit" "$axit_staged"
chmod 755 "$axit_staged"
mv -f -- "$axit_staged" "$axit_install/axit"
axit_staged=
printf 'Installed %s\n' "$axit_install/axit"
case ":$PATH:" in *":$axit_install:"*) printf '%s\n' 'Run: axit' ;; *) printf 'Run: %s/axit\nAdd %s to your PATH for the shorter command.\n' "$axit_install" "$axit_install" ;; esac
printf '%s\n' 'Automatic cleanup is off by default; existing preferences are preserved.' 'No user files were scanned, moved or deleted during installation.'

