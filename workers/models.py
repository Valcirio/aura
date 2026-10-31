"""Instalação explícita, verificada e atômica dos artefatos locais.

Somente fontes fixadas neste catálogo são aceitas. Nenhuma URL vem do LLM.
"""
import hashlib
import json
import os
from pathlib import Path
import shutil
import tempfile
import urllib.parse
import urllib.request

CATALOG = {
    "whisper-base": ("ggerganov/whisper.cpp", ["ggml-base.bin"], "MIT"),
    "pt_BR-cadu-medium": ("rhasspy/piper-voices", ["pt/pt_BR/cadu/medium/pt_BR-cadu-medium.onnx", "pt/pt_BR/cadu/medium/pt_BR-cadu-medium.onnx.json", "pt/pt_BR/cadu/medium/MODEL_CARD"], "Consulte MODEL_CARD; motor Piper GPL-3.0"),
    "pt_BR-faber-medium": ("rhasspy/piper-voices", ["pt/pt_BR/faber/medium/pt_BR-faber-medium.onnx", "pt/pt_BR/faber/medium/pt_BR-faber-medium.onnx.json", "pt/pt_BR/faber/medium/MODEL_CARD"], "Consulte MODEL_CARD; motor Piper GPL-3.0"),
    "silero-vad": (None, ["src/silero_vad/data/silero_vad.onnx"], "MIT"),
}


def request(url):
    parsed = urllib.parse.urlparse(url)
    if parsed.scheme != "https" or parsed.hostname not in {"huggingface.co", "api.github.com", "raw.githubusercontent.com"}:
        raise ValueError("Fonte de metadados não autorizada")
    return urllib.request.urlopen(urllib.request.Request(url, headers={"User-Agent": "Aura/0.2"}), timeout=30)


def metadata(url):
    with request(url) as response:
        data = response.read(2_000_001)
    if len(data) > 2_000_000:
        raise ValueError("Metadados muito grandes")
    return json.loads(data)


def verify_file(path, expected, algorithm, size):
    digest = hashlib.new(algorithm)
    if algorithm == "sha1":
        digest.update(f"blob {size}\0".encode())
    actual = hashlib.sha256()
    total = 0
    with open(path, "rb") as source:
        while chunk := source.read(1024 * 1024):
            digest.update(chunk)
            actual.update(chunk)
            total += len(chunk)
    if total != size or digest.hexdigest() != expected:
        raise ValueError("Integridade do download não confere com a revisão de origem")
    return actual.hexdigest()


def install(root, artifact, progress=lambda **event: None):
    if artifact not in CATALOG:
        raise ValueError("Modelo fora do catálogo")
    repository, files, license_label = CATALOG[artifact]
    artifact_root = Path(root) / artifact
    artifact_root.mkdir(parents=True, exist_ok=True)
    for partial in artifact_root.glob(".partial-*"):
        if partial.is_dir() and not partial.is_symlink():
            shutil.rmtree(partial)
    if repository:
        revision = metadata(f"https://huggingface.co/api/models/{repository}")["sha"]
        source_files = []
        for filename in files:
            parent = str(Path(filename).parent)
            parent = "" if parent == "." else "/" + parent
            entries = metadata(f"https://huggingface.co/api/models/{repository}/tree/{revision}{parent}?limit=1000")
            item = next(entry for entry in entries if entry["path"] == filename)
            lfs = item.get("lfs")
            source_files.append((filename, f"https://huggingface.co/{repository}/resolve/{revision}/{filename}", lfs["oid"] if lfs else item["oid"], "sha256" if lfs else "sha1", item["size"]))
    else:
        revision = metadata("https://api.github.com/repos/snakers4/silero-vad/commits/master")["sha"]
        source_files = []
        for filename in files:
            item = metadata(f"https://api.github.com/repos/snakers4/silero-vad/contents/{filename}?ref={revision}")
            source_files.append((filename, f"https://raw.githubusercontent.com/snakers4/silero-vad/{revision}/{filename}", item["sha"], "sha1", item["size"]))
    if shutil.disk_usage(artifact_root).free < sum(item[4] for item in source_files) + 8_000_000:
        raise OSError("Espaço em disco insuficiente para instalar o modelo")
    staging = Path(tempfile.mkdtemp(prefix=".partial-", dir=artifact_root))
    try:
        records = []
        for filename, url, expected, algorithm, size in source_files:
            if size > 600_000_000:
                raise ValueError("Artefato excede o limite de download")
            path = staging / Path(filename).name
            total = 0
            with request(url) as response, open(path, "wb") as target:
                while chunk := response.read(256 * 1024):
                    total += len(chunk)
                    if total > size:
                        raise ValueError("Download maior que os metadados")
                    target.write(chunk)
                    progress(artifact=artifact, downloaded=total, total=size)
                target.flush()
                os.fsync(target.fileno())
            sha256 = verify_file(path, expected, algorithm, size)
            records.append({"file": path.name, "source": filename, "sha256": sha256, "size": size})
        manifest = {"id": artifact, "revision": revision, "license": license_label, "files": records}
        (staging / "manifest.json").write_text(json.dumps(manifest), encoding="utf-8")
        target = artifact_root / revision
        if target.exists():
            # Mesma revisão: confere os arquivos existentes antes de reutilizar.
            for record in records:
                verify_file(target / record["file"], record["sha256"], "sha256", record["size"])
            shutil.rmtree(staging)
        else:
            os.replace(staging, target)
        pointer = artifact_root / ".current.partial"
        pointer.write_text(json.dumps(manifest), encoding="utf-8")
        os.replace(pointer, artifact_root / "current.json")
        return manifest
    finally:
        if staging.exists():
            shutil.rmtree(staging)


def artifact_path(root, artifact, suffix):
    if artifact not in CATALOG:
        raise ValueError("Modelo fora do catálogo")
    pointer = Path(root) / artifact / "current.json"
    if not pointer.is_file():
        raise FileNotFoundError(f"Instale {artifact} em Configurações > Modelos locais")
    manifest = json.loads(pointer.read_text())
    revision = manifest["revision"]
    if not revision.isalnum():
        raise ValueError("Revisão de modelo inválida")
    record = next(item for item in manifest["files"] if item["file"].endswith(suffix))
    filename = record["file"]
    if Path(filename).name != filename:
        raise ValueError("Caminho de modelo inválido")
    path = pointer.parent / revision / filename
    verify_file(path, record["sha256"], "sha256", record["size"])
    return path
