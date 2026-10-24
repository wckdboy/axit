#!/bin/sh
set -eu

# Local source installs require Rust; release installs need only curl, tar and SHA-256.
scry_repo=${SCRY_REPO:-wckdboy/scry}
scry_version=${SCRY_VERSION:-latest}
scry_install=${SCRY_INSTALL_DIR:-"$HOME/.local/bin"}
scry_source=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
scry_mode=${1:-release}
scry_tmp=$(mktemp -d "${TMPDIR:-/tmp}/scry-install.XXXXXXXX")
trap 'rm -rf -- "$scry_tmp"' EXIT HUP INT TERM

case "$(uname -s)/$(uname -m)" in
  Darwin/arm64) scry_target=aarch64-apple-darwin ;;
  Darwin/x86_64) scry_target=x86_64-apple-darwin ;;
  Linux/aarch64|Linux/arm64) scry_target=aarch64-unknown-linux-musl ;;
  Linux/x86_64|Linux/amd64) scry_target=x86_64-unknown-linux-musl ;;
  *) printf '%s\n' 'SCRY supports macOS/Linux on ARM64 and x86-64.' >&2; exit 1 ;;
esac

if [ "$scry_mode" = --local ]; then
  [ -f "$scry_source/Cargo.toml" ] || { printf '%s\n' 'Run --local from the SCRY source checkout.' >&2; exit 1; }
  command -v cargo >/dev/null 2>&1 || { printf '%s\n' 'Source installation requires Rust 1.88+. Release packages do not require Rust.' >&2; exit 1; }
  (cd "$scry_source" && cargo build --release --locked --jobs 2)
  cp -- "$scry_source/target/release/scry" "$scry_tmp/scry"
elif [ "$scry_mode" = --binary ]; then
  [ "$#" -eq 2 ] || { printf '%s\n' 'Usage: install.sh --binary /path/to/scry' >&2; exit 1; }
  cp -- "$2" "$scry_tmp/scry"
else
  [ "$scry_mode" = release ] || { printf '%s\n' 'Usage: install.sh [--local | --binary PATH]' >&2; exit 1; }
  scry_asset="scry-$scry_target.tar.gz"
  if command -v gh >/dev/null 2>&1 && gh auth status >/dev/null 2>&1; then
    if [ "$scry_version" = latest ]; then
      gh release download --repo "$scry_repo" --pattern "$scry_asset" --pattern SHA256SUMS --dir "$scry_tmp"
    else
      gh release download "$scry_version" --repo "$scry_repo" --pattern "$scry_asset" --pattern SHA256SUMS --dir "$scry_tmp"
    fi
  else
    command -v curl >/dev/null 2>&1 || { printf '%s\n' 'curl is required for release downloads.' >&2; exit 1; }
    if [ "$scry_version" = latest ]; then scry_base="https://github.com/$scry_repo/releases/latest/download"; else scry_base="https://github.com/$scry_repo/releases/download/$scry_version"; fi
    curl --fail --silent --show-error --location --proto '=https' "$scry_base/$scry_asset" --output "$scry_tmp/$scry_asset"
    curl --fail --silent --show-error --location --proto '=https' "$scry_base/SHA256SUMS" --output "$scry_tmp/SHA256SUMS"
  fi
  scry_expected=$(awk -v name="$scry_asset" '$2 == name {print $1}' "$scry_tmp/SHA256SUMS")
  [ "${#scry_expected}" -eq 64 ] || { printf '%s\n' 'Missing or ambiguous SHA-256 checksum.' >&2; exit 1; }
  if command -v sha256sum >/dev/null 2>&1; then scry_actual=$(sha256sum "$scry_tmp/$scry_asset" | awk '{print $1}');
  else scry_actual=$(shasum -a 256 "$scry_tmp/$scry_asset" | awk '{print $1}'); fi
  [ "$scry_actual" = "$scry_expected" ] || { printf '%s\n' 'SHA-256 verification failed; nothing installed.' >&2; exit 1; }
  scry_members=$(tar -tzf "$scry_tmp/$scry_asset")
  [ "$scry_members" = "scry
LICENSE" ] || { printf '%s\n' 'Unexpected archive contents; nothing installed.' >&2; exit 1; }
  tar -xzf "$scry_tmp/$scry_asset" -C "$scry_tmp"
  [ -f "$scry_tmp/scry" ] && [ ! -L "$scry_tmp/scry" ] || { printf '%s\n' 'Invalid binary package.' >&2; exit 1; }
fi

chmod 755 "$scry_tmp/scry"
"$scry_tmp/scry" --version
mkdir -p -- "$scry_install"
[ ! -L "$scry_install/scry" ] || { printf '%s\n' 'Existing scry is a symlink; choose SCRY_INSTALL_DIR explicitly.' >&2; exit 1; }
[ ! -e "$scry_install/scry" ] || [ -f "$scry_install/scry" ] || { printf '%s\n' 'Existing scry is not a regular file; nothing installed.' >&2; exit 1; }
cp -- "$scry_tmp/scry" "$scry_install/.scry-new-$$"
chmod 755 "$scry_install/.scry-new-$$"
mv -f -- "$scry_install/.scry-new-$$" "$scry_install/scry"
printf 'Installed %s\n' "$scry_install/scry"
case ":$PATH:" in *":$scry_install:"*) printf '%s\n' 'Run: scry' ;; *) printf 'Run: %s/scry\nAdd %s to your PATH for the shorter command.\n' "$scry_install" "$scry_install" ;; esac
printf '%s\n' 'Automatic cleanup is off. No user files were scanned, moved or deleted during installation.'

