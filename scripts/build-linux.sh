#!/usr/bin/env bash
set -euo pipefail
aura_root="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$aura_root"
export PATH="$aura_root/.tools/bin:$PATH"
if [[ ! -x "$aura_root/workers/python/bin/python3" ]] || [[ ! -d "$aura_root/workers/python-packages/piper" ]]; then
  echo 'Execute bash scripts/setup-audio.sh antes de empacotar: o pacote deve conter Python e os motores locais.' >&2
  exit 1
fi
cargo test --locked --manifest-path crates/support-core/Cargo.toml
cargo test --locked --manifest-path crates/support-runtime/Cargo.toml
python3 -m unittest discover -s workers -p 'test_*.py'
cargo tauri build
