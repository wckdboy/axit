#!/bin/sh
set -eu
scry_binary=${1:?Usage: installer-test.sh BINARY}
scry_test_tmp=$(mktemp -d)
trap 'rm -rf -- "$scry_test_tmp"' EXIT HUP INT TERM
SCRY_INSTALL_DIR="$scry_test_tmp/bin" sh install.sh --binary "$scry_binary"
"$scry_test_tmp/bin/scry" --version
test -x "$scry_test_tmp/bin/scry"
test ! -e "$scry_test_tmp/bin/.scry-new-$$"
printf '%s\n' 'Installer fixture passed; no shell startup files changed.'
