#!/usr/bin/env python3
"""Aggregate audio-bench results (stdlib only): ASR WER/latency, TTS and VAD summaries.

Reads  <data>/results/asr-*.json, tts-*.json, vad-*.json and <data>/corpus/references.json.
Writes <data>/results/summary.md and prints it. ASR files from any engine (sherpa-onnx, candle...)
are accepted as long as they follow the schema: engine, model, threads, load_ms, p50_ms, p95_ms,
mean_rtf, peak_rss_mb (top level or under "summary") and clips[{id, text, ...}].
"""
import argparse
import datetime
import json
import sys
import unicodedata
from pathlib import Path

HERE = Path(__file__).resolve().parent


def normalize(text, strip_accents=False):
    t = unicodedata.normalize("NFC", text).lower()
    out = []
    for ch in t:
        cat = unicodedata.category(ch)
        if cat == "Pd":
            out.append(" ")
        elif cat[0] in "PS":
            continue
        else:
            out.append(ch)
    t = "".join(out)
    if strip_accents:
        t = "".join(c for c in unicodedata.normalize("NFD", t) if unicodedata.category(c) != "Mn")
        t = unicodedata.normalize("NFC", t)
    return " ".join(t.split())


def edit_distance(ref, hyp):
    prev = list(range(len(hyp) + 1))
    for i, rw in enumerate(ref, 1):
        cur = [i] + [0] * len(hyp)
        for j, hw in enumerate(hyp, 1):
            cur[j] = min(prev[j] + 1, cur[j - 1] + 1, prev[j - 1] + (rw != hw))
        prev = cur
    return prev[-1]


def wer(run, refs, strip_accents):
    """Corpus-level WER (sum errors / sum ref words) and per-clip WER dict."""
    errs = words = 0
    per_clip = {}
    for c in run.get("clips", []):
        if c["id"] not in refs:
            continue
        r = normalize(refs[c["id"]], strip_accents).split()
        h = normalize(c.get("text", ""), strip_accents).split()
        e = edit_distance(r, h)
        errs += e
        words += len(r)
        per_clip[c["id"]] = e / len(r) if r else 0.0
    return (errs / words if words else None), per_clip


def get(d, key):
    if key in d:
        return d[key]
    return (d.get("summary") or {}).get(key)


def fnum(x, nd=1):
    return "-" if x is None else f"{x:.{nd}f}"


def pct(x):
    return "-" if x is None else f"{100 * x:.1f}%"


def table(headers, rows):
    out = ["| " + " | ".join(headers) + " |", "|" + "|".join("---" for _ in headers) + "|"]
    out += ["| " + " | ".join(str(c) for c in r) + " |" for r in rows]
    return "\n".join(out)


def load_runs(results, pattern):
    runs = []
    for p in sorted(results.glob(pattern)):
        try:
            d = json.loads(p.read_text(encoding="utf-8"))
            d["_file"] = p.name
            runs.append(d)
        except (OSError, json.JSONDecodeError) as e:
            print(f"warning: skipping {p}: {e}", file=sys.stderr)
    return runs


def label(r):
    m = str(get(r, "model") or "?")
    v = get(r, "variant") or get(r, "dtype")
    if v:
        m += f" ({v})"
    return f"{get(r, 'engine') or '?'}/{m}/t{get(r, 'threads')}"


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--data-dir", type=Path, default=HERE / "data")
    args = ap.parse_args()
    data = args.data_dir
    results = data / "results"
    refs_list = json.loads((data / "corpus" / "references.json").read_text(encoding="utf-8"))
    refs = {r["id"]: r["text"] for r in refs_list}

    md = [f"# audio-bench — resumo ({datetime.datetime.now():%Y-%m-%d %H:%M})", ""]

    # ---- ASR
    asr = load_runs(results, "asr-*.json")
    asr.sort(key=lambda r: (str(get(r, "engine")), str(get(r, "model")), get(r, "threads") or 0))
    md += ["## ASR (pt-BR, Common Voice 17 test)", ""]
    if not asr:
        md += ["_nenhum resultado asr-*.json_", ""]
    else:
        rows, wers = [], {}
        for r in asr:
            w, pc = wer(r, refs, False)
            wa, pca = wer(r, refs, True)
            wers[label(r)] = (pc, pca)
            m = str(get(r, "model"))
            if get(r, "variant") or get(r, "dtype"):
                m += f" ({get(r, 'variant') or get(r, 'dtype')})"
            rows.append([get(r, "engine"), m, get(r, "threads"), fnum(get(r, "load_ms"), 0),
                         fnum(get(r, "p50_ms"), 0), fnum(get(r, "p95_ms"), 0), fnum(get(r, "mean_rtf"), 3),
                         fnum(get(r, "peak_rss_mb"), 0), pct(w), pct(wa)])
        md += [table(["engine", "model", "threads", "load_ms", "p50 ms", "p95 ms", "mean RTF",
                      "peak RSS MB", "WER", "WER (sem acento)"], rows), "",
               "WER corpus-level; normalização: NFC, minúsculas, pontuação removida (hífen vira espaço), "
               "espaços colapsados. Números por extenso vs. dígitos não são normalizados. "
               "Whisper processa janela fixa de 30 s: o RTF de clipes curtos é pessimista.", ""]
        labels = [label(r) for r in asr]
        crow = []
        for cid in refs:
            crow.append([cid] + [pct(wers[l][0].get(cid)) for l in labels])
        md += ["### WER por clipe", "", table(["clip"] + labels, crow), "", "### Transcrições", ""]
        for cid, text in refs.items():
            md.append(f"- **{cid}** REF: {text}")
            for r in asr:
                hyp = next((c.get("text", "") for c in r.get("clips", []) if c["id"] == cid), "")
                md.append(f"  - {label(r)}: {hyp}")
        md.append("")

    # ---- TTS
    tts = load_runs(results, "tts-*.json")
    md += ["## TTS (pt-BR)", ""]
    if not tts:
        md += ["_nenhum resultado tts-*.json_", ""]
    else:
        rows = [[get(r, "engine"), get(r, "voice"), get(r, "speaker") or "—", get(r, "threads"),
                 fnum(get(r, "load_ms"), 0), get(r, "sample_rate"), fnum(get(r, "mean_rtf"), 3),
                 fnum(get(r, "peak_rss_mb"), 0), get(r, "license") or "—"]
                for r in tts]
        md += [table(["engine", "voice", "speaker", "threads", "load_ms", "sample rate", "mean RTF",
                      "peak RSS MB", "license"], rows), ""]
        for r in tts:
            srows = [[s["idx"], s["chars"], fnum(s["audio_s"], 2), fnum(s["phonemize_ms"], 0),
                      fnum(s["synth_ms"], 0), fnum(s["rtf"], 3), fnum(s.get("rms"), 3)]
                     for s in r.get("sentences", [])]
            md += [f"### {get(r, 'engine')}/{get(r, 'voice')}", "",
                   table(["#", "chars", "áudio s", "phonemize ms", "synth ms", "RTF", "RMS"], srows), ""]

    # ---- VAD
    vad = load_runs(results, "vad-*.json")
    md += ["## VAD (Silero v5)", ""]
    if not vad:
        md += ["_nenhum resultado vad-*.json_", ""]
    else:
        rows = [[get(r, "engine"), get(r, "model"), fnum(get(r, "mean_us_per_frame"), 1),
                 (get(r, "silence") or {}).get("num_segments"), (get(r, "silence") or {}).get("ok"),
                 fnum(get(r, "peak_rss_mb"), 0)] for r in vad]
        md += [table(["engine", "model", "µs / frame (512)", "segmentos em silêncio", "silêncio OK", "peak RSS MB"], rows), ""]
        for r in vad:
            crows = []
            for c in r.get("clips", []):
                spans = ", ".join(f"{s['start_s']:.2f}–{s['end_s']:.2f}" for s in c.get("segments", []))
                crows.append([c["id"], fnum(c.get("duration_s"), 2), c.get("num_segments"),
                              fnum(c.get("process_ms"), 1), fnum(c.get("us_per_frame"), 1), spans])
            md += [f"### {get(r, 'engine')}/{get(r, 'model')}", "",
                   table(["clip", "dur s", "segmentos", "proc ms", "µs/frame", "intervalos (s)"], crows), ""]

    text = "\n".join(md) + "\n"
    results.mkdir(parents=True, exist_ok=True)
    (results / "summary.md").write_text(text, encoding="utf-8")
    print(text)


if __name__ == "__main__":
    main()
