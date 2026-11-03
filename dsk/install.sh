#!/usr/bin/env sh
# Build dsk from source and install to ~/.local/bin (works on Debian/Ubuntu, Arch, Fedora).
set -e
cd "$(dirname "$0")"
if ! command -v cargo >/dev/null 2>&1; then
  [ -x "$HOME/.cargo/bin/cargo" ] || curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --profile minimal
  PATH="$HOME/.cargo/bin:$PATH"
fi
command -v cc >/dev/null 2>&1 || { echo "Need a C linker: apt install build-essential | pacman -S base-devel | dnf install gcc"; exit 1; }
cargo build --release
mkdir -p "$HOME/.local/bin"
install -m 755 target/release/dsk "$HOME/.local/bin/dsk"
echo "Installed: $HOME/.local/bin/dsk"
case ":$PATH:" in *":$HOME/.local/bin:"*) ;; *) echo "Add ~/.local/bin to your PATH" ;; esac
