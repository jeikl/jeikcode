#!/usr/bin/env bash
# Build the full jeikcode product with the `dbg` profile (see workspace Cargo.toml).
# Usage (repo root):
#   ./scripts/debug-jeikcode.sh
#   ./scripts/debug-jeikcode.sh init --force
#
# Output: target/dbg/jeikcode  (not target/debug — that cache stays for tests)

set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"
echo "==> cargo build -p atomcode --bin jeikcode --profile dbg"
cargo build -p atomcode --bin jeikcode --profile dbg
EXE="$ROOT/target/dbg/jeikcode"
echo "==> $EXE"
if [[ $# -gt 0 ]]; then
  echo "==> running: jeikcode $*"
  exec "$EXE" "$@"
fi
echo "    gdb:  rust-gdb --args $EXE init --force"
echo "    or:   $EXE init --force"
