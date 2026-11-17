#!/usr/bin/env bash
# Usage: scripts/install.sh
# Builds dbzilla in release mode and copies the executable to ~/.local/bin/.
set -euo pipefail

BIN_NAME="dbzilla"
DEST_DIR="${HOME}/.local/bin"

cd "$(dirname "$0")/.."

cargo build --release -p dbzilla-app

mkdir -p "$DEST_DIR"
cp "target/release/${BIN_NAME}" "${DEST_DIR}/${BIN_NAME}"

echo "installed: ${DEST_DIR}/${BIN_NAME}"
