#!/usr/bin/env bash
set -euo pipefail
aura_root="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
if ! command -v uv >/dev/null; then
  echo 'Instale uv (https://docs.astral.sh/uv/getting-started/installation/) e execute este script novamente.' >&2
  exit 1
fi
export UV_CACHE_DIR="$aura_root/.runtime/uv-cache"
uv python install --no-bin --install-dir "$aura_root/.runtime/python" 3.12
aura_python="$(find "$aura_root/.runtime/python" -path '*/bin/python3.12' -type f -print -quit)"
if [[ -z "$aura_python" ]]; then
  echo 'Python 3.12 gerenciado não encontrado.' >&2
  exit 1
fi
aura_prefix="$(dirname -- "$(dirname -- "$aura_python")")"
mkdir -p "$aura_root/workers/python"
cp -a "$aura_prefix/." "$aura_root/workers/python/"
uv pip compile --python-version 3.12 --generate-hashes "$aura_root/workers/requirements.in" --output-file "$aura_root/workers/requirements.lock"
uv pip install --python "$aura_root/workers/python/bin/python3.12" --target "$aura_root/workers/python-packages" --require-hashes --requirements "$aura_root/workers/requirements.lock"
echo 'Motores de áudio instalados no projeto. Instale os modelos pela página Configurações do Aura.'
