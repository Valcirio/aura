#!/usr/bin/env python3
"""Baixa modelos e corpus pt-BR para o benchmark de ASR/TTS (somente stdlib).

Uso: python3 tools/audio-bench/fetch.py [--repin] [--refresh-corpus] [--skip-corpus]
Tudo vai para tools/audio-bench/data/ (o safetensors grande fica em .models-cache/
na raiz do repo e e' linkado simbolicamente). Gera data/manifest.json.
"""
import argparse
import hashlib
import http.client
import json
import os
import shutil
import subprocess
import sys
import time
import urllib.error
import urllib.parse
import urllib.request
import wave
from datetime import datetime, timezone
from pathlib import Path

HERE = Path(__file__).resolve().parent
ROOT = HERE.parent.parent
DATA = HERE / "data"
MODELS = DATA / "models"
CORPUS = DATA / "corpus"
CACHE = ROOT / ".models-cache"
MANIFEST = DATA / "manifest.json"
HF = "https://huggingface.co"
UA = "aura-audio-bench/1.0"
TIMEOUT = 120  # segundos por operacao de socket

HF_JOBS = [
    # (repo, arquivos, subdir de data/models)
    ("csukuangfj/sherpa-onnx-whisper-turbo",
     ["turbo-encoder.int8.onnx", "turbo-decoder.int8.onnx", "turbo-tokens.txt"],
     "whisper-turbo"),
    ("csukuangfj/sherpa-onnx-whisper-small",
     ["small-encoder.int8.onnx", "small-decoder.int8.onnx", "small-tokens.txt"],
     "whisper-small"),
    ("csukuangfj/vits-piper-pt_BR-cadu-medium",
     ["pt_BR-cadu-medium.onnx", "pt_BR-cadu-medium.onnx.json", "tokens.txt"],
     "piper-cadu"),
    ("csukuangfj/vits-piper-pt_BR-faber-medium",
     ["pt_BR-faber-medium.onnx", "pt_BR-faber-medium.onnx.json", "tokens.txt"],
     "piper-faber"),
    ("openai/whisper-large-v3-turbo",
     ["config.json", "tokenizer.json", "preprocessor_config.json"],
     "whisper-turbo-hf"),
]
SAFETENSORS_REPO = "openai/whisper-large-v3-turbo"
SAFETENSORS_FILE = "model.safetensors"
SAFETENSORS_CACHE = CACHE / "whisper-large-v3-turbo-hf" / SAFETENSORS_FILE
SAFETENSORS_LINK = MODELS / "whisper-turbo-hf" / SAFETENSORS_FILE

CANDLE_REPO = "huggingface/candle"
CANDLE_COMMIT = "2feb0b054f96e4c4c87f01b243e749896c94f8c7"
CANDLE_PATH = "candle-examples/examples/whisper/melfilters128.bytes"
SILERO_REPO = "snakers4/silero-vad"
SILERO_REF = "v5.0"
SILERO_PATH = "files/silero_vad.onnx"

CV_DATASET = "fixie-ai/common_voice_17_0"
CV_CONFIG = "pt"
CV_SPLIT = "test"
N_CLIPS = 12
MIN_CLIPS_OK = 10
MIN_S, MAX_S = 3.0, 12.0
MIN_CH, MAX_CH = 35, 140
PAGE = 20
MAX_OFFSET = 600


def log(msg):
    print(msg, flush=True)


# ---------------------------------------------------------------- HTTP
def http_open(url, headers=None):
    h = {"User-Agent": UA}
    h.update(headers or {})
    return urllib.request.urlopen(urllib.request.Request(url, headers=h), timeout=TIMEOUT)


def gh_headers():
    tok = os.environ.get("GITHUB_TOKEN")
    h = {"Accept": "application/vnd.github+json"}
    if tok:
        h["Authorization"] = f"Bearer {tok}"
    return h


def get_json(url, headers=None, retries=6):
    last = None
    for i in range(retries):
        try:
            with http_open(url, headers) as r:
                return json.load(r)
        except urllib.error.HTTPError as e:
            if e.code in (401, 403, 404):
                raise RuntimeError(f"GET {url} -> HTTP {e.code}") from e
            last = e
        except (urllib.error.URLError, OSError, http.client.HTTPException,
                json.JSONDecodeError) as e:
            last = e
        time.sleep(min(2 ** i, 30))
    raise RuntimeError(f"GET {url} falhou apos {retries} tentativas: {last}")


def download(url, dest, expected_size=None):
    """Download retomavel (.part + Range). Se expected_size for None, aceita o que vier."""
    dest.parent.mkdir(parents=True, exist_ok=True)
    part = dest.with_name(dest.name + ".part")
    done = False
    for attempt in range(1, 10):
        have = part.stat().st_size if part.exists() else 0
        if expected_size is not None:
            if have > expected_size:
                part.unlink()
                have = 0
            if have == expected_size:
                done = True
                break
        try:
            headers = {"Range": f"bytes={have}-"} if have else {}
            with http_open(url, headers) as r:
                if have and getattr(r, "status", 200) != 206:
                    have = 0  # servidor ignorou o Range: recomeca
                total = expected_size
                t0 = last = time.time()
                got = have
                with open(part, "ab" if have else "wb") as f:
                    while True:
                        chunk = r.read(1 << 20)
                        if not chunk:
                            break
                        f.write(chunk)
                        got += len(chunk)
                        if total and total > 50_000_000 and time.time() - last > 5:
                            last = time.time()
                            print(f"\r    {dest.name}: {got * 100 // total}% "
                                  f"({got / 1e6:.0f}/{total / 1e6:.0f} MB, "
                                  f"{(time.time() - t0):.0f}s)", end="", file=sys.stderr, flush=True)
            if total and total > 50_000_000:
                print("", file=sys.stderr)
            if expected_size is None:
                done = True
                break
        except urllib.error.HTTPError as e:
            if e.code == 416 and have:
                done = True  # ja completo
                break
            if e.code in (401, 403, 404):
                raise RuntimeError(f"download {url} -> HTTP {e.code}") from e
            log(f"    tentativa {attempt}: HTTP {e.code}; repetindo")
            time.sleep(min(2 ** attempt, 30))
        except (urllib.error.URLError, OSError, http.client.HTTPException) as e:
            log(f"    tentativa {attempt}: {e}; repetindo")
            time.sleep(min(2 ** attempt, 30))
    if not done:
        raise RuntimeError(f"download de {url} falhou")
    if expected_size is not None and part.stat().st_size != expected_size:
        raise RuntimeError(f"tamanho inesperado em {part}: {part.stat().st_size} != {expected_size}")
    os.replace(part, dest)


# ---------------------------------------------------------------- hashes
def sha256_file(p):
    h = hashlib.sha256()
    with open(p, "rb") as f:
        while chunk := f.read(1 << 20):
            h.update(chunk)
    return h.hexdigest()


def git_blob_sha1(p):
    size = Path(p).stat().st_size
    h = hashlib.sha1()
    h.update(f"blob {size}\0".encode())
    with open(p, "rb") as f:
        while chunk := f.read(1 << 20):
            h.update(chunk)
    return h.hexdigest()


def ensure_file(url, dest, size=None, sha256=None, git_sha1=None):
    """Baixa se ausente/invalido, verifica hashes. Retorna sha256 calculado."""
    if dest.exists():
        ok = size is None or dest.stat().st_size == size
        if ok:
            d = sha256_file(dest)
            ok = (sha256 is None or d == sha256) and \
                 (git_sha1 is None or git_blob_sha1(dest) == git_sha1)
            if ok:
                log(f"  [ok]   {dest.relative_to(DATA) if DATA in dest.parents else dest}")
                return d
        log(f"  [!]    {dest.name} invalido, rebaixando")
        dest.unlink()
    log(f"  [get]  {url}")
    download(url, dest, size)
    d = sha256_file(dest)
    if sha256 is not None and d != sha256:
        dest.unlink()
        raise RuntimeError(f"sha256 divergente em {dest.name}: {d} != {sha256}")
    if git_sha1 is not None and git_blob_sha1(dest) != git_sha1:
        dest.unlink()
        raise RuntimeError(f"git blob sha1 divergente em {dest.name}")
    return d


def rel(p):
    return str(Path(p).relative_to(DATA))


def entry(source, repo, revision, file, path, size, sha, sha_source, url):
    return {"source": source, "repo": repo, "revision": revision, "file": file,
            "path": path, "size": size, "sha256": sha, "sha256_source": sha_source, "url": url}


# ---------------------------------------------------------------- Hugging Face
def hf_repo_files(repo, pinned):
    rev = pinned or get_json(f"{HF}/api/models/{repo}")["sha"]
    tree = get_json(f"{HF}/api/models/{repo}/tree/{rev}")
    return rev, {e["path"]: e for e in tree if e.get("type") == "file"}


def hf_fetch(repo, rev, files, name, dest):
    e = files.get(name)
    if e is None:
        raise RuntimeError(f"{repo}@{rev[:8]} nao tem {name}")
    url = f"{HF}/{repo}/resolve/{rev}/{urllib.parse.quote(name)}"
    lfs = e.get("lfs")
    if lfs:
        d = ensure_file(url, dest, size=e["size"], sha256=lfs["oid"])
        src = "lfs"
    else:
        d = ensure_file(url, dest, size=e["size"], git_sha1=e["oid"])
        src = "computed+git-blob-sha1-verified"
    return d, e["size"], src, url


# ---------------------------------------------------------------- GitHub
def gh_contents(repo, path, ref):
    return get_json(f"https://api.github.com/repos/{repo}/contents/{path}?ref={ref}", gh_headers())


def gh_fetch(repo, commit, path, dest, prev_sha):
    meta = gh_contents(repo, path, commit)
    url = f"https://raw.githubusercontent.com/{repo}/{commit}/{path}"
    d = ensure_file(url, dest, size=meta["size"], sha256=prev_sha, git_sha1=meta["sha"])
    return d, meta["size"], url


# ---------------------------------------------------------------- corpus
def wav_info(p):
    with wave.open(str(p), "rb") as w:
        return w.getframerate(), w.getnchannels(), w.getsampwidth(), w.getnframes() / w.getframerate()


def ffmpeg_to_wav(src, dst):
    subprocess.run(["ffmpeg", "-y", "-loglevel", "error", "-i", str(src), "-ac", "1", "-ar", "16000",
                    "-c:a", "pcm_s16le", "-map_metadata", "-1", "-fflags", "+bitexact",
                    "-f", "wav", str(dst)], check=True)


def corpus_ok():
    refs = CORPUS / "references.json"
    if not refs.exists():
        return None
    try:
        data = json.loads(refs.read_text(encoding="utf-8"))
        if len(data) >= MIN_CLIPS_OK and all((CORPUS / Path(r["file"]).name).exists() for r in data):
            return data
    except Exception:
        pass
    return None


def build_corpus():
    if shutil.which("ffmpeg") is None:
        raise RuntimeError("ffmpeg nao encontrado no PATH")
    ds_rev = get_json(f"{HF}/api/datasets/{CV_DATASET}")["sha"]
    log(f"  dataset {CV_DATASET}@{ds_rev[:8]} config={CV_CONFIG} split={CV_SPLIT}")
    for old in list(CORPUS.glob("*.wav")) + list(CORPUS.glob("_tmp*")):
        old.unlink()
    CORPUS.mkdir(parents=True, exist_ok=True)
    picked, seen_clients, seen_text, entries = [], set(), set(), []
    tmp_mp3, tmp_wav = CORPUS / "_tmp.mp3", CORPUS / "_tmp.wav"
    offset = 0
    try:
        while len(picked) < N_CLIPS and offset < MAX_OFFSET:
            q = urllib.parse.urlencode({"dataset": CV_DATASET, "config": CV_CONFIG,
                                        "split": CV_SPLIT, "offset": offset, "length": PAGE})
            page = get_json(f"https://datasets-server.huggingface.co/rows?{q}")
            rows = page.get("rows", [])
            if not rows:
                break
            for r in rows:
                if len(picked) >= N_CLIPS:
                    break
                row = r["row"]
                text = (row.get("sentence") or "").strip()
                cid = row.get("client_id")
                if not (MIN_CH <= len(text) <= MAX_CH) or row.get("down_votes", 0) > 0:
                    continue
                if cid in seen_clients or text in seen_text:
                    continue
                src = row["audio"][0]["src"]
                download(src, tmp_mp3)
                ffmpeg_to_wav(tmp_mp3, tmp_wav)
                sr, ch, sw, dur = wav_info(tmp_wav)
                if not (MIN_S <= dur <= MAX_S):
                    continue
                n = len(picked) + 1
                out = CORPUS / f"{n:02d}.wav"
                os.replace(tmp_wav, out)
                seen_clients.add(cid)
                seen_text.add(text)
                orig = Path(row.get("path") or "").name
                picked.append({"id": f"{n:02d}", "file": f"{n:02d}.wav", "text": text,
                               "duration_s": round(dur, 2),
                               "source": {"dataset": CV_DATASET, "revision": ds_rev,
                                          "config": CV_CONFIG, "split": CV_SPLIT,
                                          "row_idx": r["row_idx"], "original": orig}})
                entries.append(entry("datasets-server", CV_DATASET, ds_rev,
                                     orig or f"row {r['row_idx']}", f"corpus/{n:02d}.wav",
                                     out.stat().st_size, sha256_file(out), "computed",
                                     "https://datasets-server.huggingface.co/rows"))
                log(f"  [clip] {n:02d}.wav {dur:.1f}s  {text[:60]}")
            offset += PAGE
    finally:
        for t in (tmp_mp3, tmp_wav):
            if t.exists():
                t.unlink()
    if len(picked) < MIN_CLIPS_OK:
        raise RuntimeError(f"so {len(picked)} clipes validos (minimo {MIN_CLIPS_OK})")
    refs = CORPUS / "references.json"
    refs.write_text(json.dumps(picked, ensure_ascii=False, indent=2) + "\n", encoding="utf-8")
    entries.append(entry("generated", CV_DATASET, ds_rev, "references.json", "corpus/references.json",
                         refs.stat().st_size, sha256_file(refs), "computed", ""))
    return entries


# ---------------------------------------------------------------- main
def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--repin", action="store_true", help="re-resolve revisoes HF/GitHub (ignora pins do manifest)")
    ap.add_argument("--refresh-corpus", action="store_true", help="refaz o corpus")
    ap.add_argument("--skip-corpus", action="store_true")
    args = ap.parse_args()

    prev = {}
    if MANIFEST.exists() and not args.repin:
        try:
            prev = json.loads(MANIFEST.read_text(encoding="utf-8"))
        except Exception:
            prev = {}
    prev_files = {f["path"]: f for f in prev.get("files", [])}
    prev_rev = {}
    for f in prev.get("files", []):
        prev_rev.setdefault(f["repo"], f["revision"])

    DATA.mkdir(parents=True, exist_ok=True)
    files_out = []
    repo_info = {}

    for repo, names, sub in HF_JOBS:
        log(f"== {repo} -> models/{sub}")
        rev, tree = hf_repo_files(repo, prev_rev.get(repo))
        repo_info[repo] = (rev, tree)
        for n in names:
            dest = MODELS / sub / n
            d, size, src, url = hf_fetch(repo, rev, tree, n, dest)
            files_out.append(entry("huggingface", repo, rev, n, rel(dest), size, d, src, url))

    # safetensors: cache fora do repo de dados + symlink relativo
    log(f"== {SAFETENSORS_REPO}/{SAFETENSORS_FILE} -> .models-cache + symlink")
    rev, tree = repo_info[SAFETENSORS_REPO]
    d, size, src, url = hf_fetch(SAFETENSORS_REPO, rev, tree, SAFETENSORS_FILE, SAFETENSORS_CACHE)
    SAFETENSORS_LINK.parent.mkdir(parents=True, exist_ok=True)
    link_target = os.path.relpath(SAFETENSORS_CACHE, SAFETENSORS_LINK.parent)
    if SAFETENSORS_LINK.is_symlink() or SAFETENSORS_LINK.exists():
        if SAFETENSORS_LINK.is_symlink() and os.readlink(SAFETENSORS_LINK) == link_target:
            pass
        else:
            SAFETENSORS_LINK.unlink()
    if not SAFETENSORS_LINK.is_symlink():
        os.symlink(link_target, SAFETENSORS_LINK)
    log(f"  symlink {SAFETENSORS_LINK.relative_to(ROOT)} -> {link_target}")
    files_out.append(entry("huggingface", SAFETENSORS_REPO, rev, SAFETENSORS_FILE,
                           rel(SAFETENSORS_LINK), size, d, src, url))

    # candle mel filters
    log(f"== {CANDLE_REPO}@{CANDLE_COMMIT[:8]} melfilters128.bytes")
    dest = MODELS / "whisper-turbo-hf" / "melfilters128.bytes"
    d, size, url = gh_fetch(CANDLE_REPO, CANDLE_COMMIT, CANDLE_PATH, dest,
                            prev_files.get(rel(dest), {}).get("sha256"))
    files_out.append(entry("github", CANDLE_REPO, CANDLE_COMMIT, CANDLE_PATH, rel(dest), size, d,
                           "computed+git-blob-sha1-verified", url))

    # silero vad v5 (upstream)
    log(f"== {SILERO_REPO}@{SILERO_REF} silero_vad.onnx")
    commit = prev_rev.get(SILERO_REPO) or \
        get_json(f"https://api.github.com/repos/{SILERO_REPO}/commits/{SILERO_REF}", gh_headers())["sha"]
    dest = MODELS / "silero-vad" / "silero_vad.onnx"
    d, size, url = gh_fetch(SILERO_REPO, commit, SILERO_PATH, dest,
                            prev_files.get(rel(dest), {}).get("sha256"))
    files_out.append(entry("github", SILERO_REPO, commit, SILERO_PATH, rel(dest), size, d,
                           "computed+git-blob-sha1-verified", url))

    # corpus
    if not args.skip_corpus:
        log(f"== corpus pt-BR ({CV_DATASET})")
        existing = None if args.refresh_corpus else corpus_ok()
        if existing and prev.get("files"):
            log(f"  corpus ja presente ({len(existing)} clipes), mantendo")
            files_out += [f for f in prev["files"] if f["path"].startswith("corpus/")]
        else:
            files_out += build_corpus()

    manifest = {"version": 1,
                "generated_at": datetime.now(timezone.utc).isoformat(timespec="seconds"),
                "files": files_out}
    tmp = MANIFEST.with_name("manifest.json.tmp")
    tmp.write_text(json.dumps(manifest, indent=2) + "\n", encoding="utf-8")
    os.replace(tmp, MANIFEST)
    log(f"\nOK: {len(files_out)} arquivos no manifest -> {MANIFEST.relative_to(ROOT)}")


if __name__ == "__main__":
    try:
        main()
    except KeyboardInterrupt:
        sys.exit(130)
    except Exception as exc:
        print(f"ERRO: {exc}", file=sys.stderr)
        sys.exit(1)
