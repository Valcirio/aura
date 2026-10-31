#!/usr/bin/env bash
set -euo pipefail
aura_root="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$aura_root"
export PATH="$aura_root/.tools/bin:$PATH"
if ! command -v trunk >/dev/null || ! command -v cargo-tauri >/dev/null; then
  echo 'Prepare as ferramentas:' >&2
  echo '  rustup target add wasm32-unknown-unknown' >&2
  echo '  cargo install trunk --locked --root .tools' >&2
  echo "  cargo install tauri-cli --version '^2' --locked --root .tools" >&2
  exit 1
fi
pkg-config --exists alsa gtk+-3.0 webkit2gtk-4.1 || {
  echo 'Instale as dependências nativas de Tauri e ALSA. Consulte docs/EXECUCAO.md.' >&2
  exit 1
}
cargo tauri dev
