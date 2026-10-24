#!/bin/bash
# SessionStart hook for Claude Code cloud sessions: installs what `cargo xtask ci` needs.
set -euo pipefail

if [ "${CLAUDE_CODE_REMOTE:-}" != "true" ]; then
  exit 0
fi

cd "${CLAUDE_PROJECT_DIR:-$(dirname "$0")/../..}"

# Linux build dependencies (winit, wgpu, rfd), same list as .github/workflows/ci.yml.
pkgs="pkg-config libxkbcommon-dev libwayland-dev libx11-dev libxcursor-dev libxrandr-dev
  libxi-dev libgtk-3-dev libfontconfig1-dev libfreetype-dev mesa-vulkan-drivers"
missing=""
for p in $pkgs; do
  dpkg -s "$p" >/dev/null 2>&1 || missing="$missing $p"
done
if [ -n "$missing" ]; then
  SUDO=""
  [ "$(id -u)" -ne 0 ] && SUDO="sudo"
  $SUDO apt-get update -qq
  DEBIAN_FRONTEND=noninteractive $SUDO apt-get install -y -qq --no-install-recommends $missing
fi

# Rust components and the wasm target (`cargo xtask wasm`).
rustup component add clippy rustfmt >/dev/null
rustup target add wasm32-unknown-unknown >/dev/null

# Warm the build cache so tests and clippy start fast.
cargo fetch --locked
cargo build --workspace --all-targets --locked -q
