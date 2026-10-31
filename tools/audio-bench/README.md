# audio-bench

Benchmark fora do app para comparar ASR local em pt-BR (sherpa-onnx Whisper turbo/small int8 vs
candle Whisper large-v3-turbo), além de TTS Piper pt_BR (cadu/faber) e VAD Silero v5.
Esta pasta contém a preparação de modelos/corpus (`fetch.py`) e o crate Rust `audio-bench` (engines sherpa-onnx) + `report.py`.

## Uso

    python3 tools/audio-bench/fetch.py          # baixa tudo (~2 GB), idempotente e retomável
    python3 tools/audio-bench/fetch.py --repin  # re-resolve revisões (ignora pins do manifest)
    python3 tools/audio-bench/fetch.py --refresh-corpus

Somente stdlib do Python 3 + `ffmpeg` no PATH. Revisões (commit sha) e SHA-256 ficam em
`data/manifest.json`.

## Layout de `data/` (ignorado pelo git)

    data/manifest.json
    data/models/whisper-turbo/      encoder/decoder int8 + tokens (sherpa)
    data/models/whisper-small/      idem, small
    data/models/silero-vad/         silero_vad.onnx (upstream v5.0)
    data/models/piper-cadu/         pt_BR-cadu-medium.onnx(.json) + tokens.txt
    data/models/piper-faber/        pt_BR-faber-medium.onnx(.json) + tokens.txt
    data/models/whisper-turbo-hf/   config/tokenizer/preprocessor, melfilters128.bytes,
                                    model.safetensors (symlink -> ../../../../../.models-cache/...)
    data/corpus/NN.wav              16 kHz mono PCM s16le (Common Voice 17 pt, split test)
    data/corpus/references.json     [{id,file,text,duration_s,source}]

`.models-cache/` (raiz do repo) guarda o `model.safetensors` (1,6 GB) e o jar nativo do sherpa-onnx.

## Benchmarks

Crate Rust independente (`[workspace]` próprio, não faz parte do workspace raiz). Rodar a partir de
`tools/audio-bench`, cada processo separado (VmHWM por execução), sem outra carga pesada em paralelo:

    cd tools/audio-bench
    cargo build --release        # 1º build baixa libs pré-compiladas do sherpa-onnx e piper-phonemize
    ./target/release/asr-sherpa --model turbo --threads 8 --corpus data/corpus --out data/results/asr-sherpa-turbo-t8.json
    ./target/release/asr-sherpa --model turbo --threads 4 --corpus data/corpus --out data/results/asr-sherpa-turbo-t4.json
    ./target/release/asr-sherpa --model small --threads 8 --corpus data/corpus --out data/results/asr-sherpa-small-t8.json
    ./target/release/asr-sherpa --model small --threads 4 --corpus data/corpus --out data/results/asr-sherpa-small-t4.json
    ./target/release/vad-sherpa --corpus data/corpus
    ./target/release/tts-sherpa --voice cadu --threads 4 --out-dir data/out
    ./target/release/tts-sherpa --voice faber --threads 4 --out-dir data/out
    python3 report.py            # gera data/results/summary.md (WER, latência, RTF, RSS)

    # candle (feature opcional, fora do default; CPU puro, sem MKL, RUSTFLAGS padrão):
    cargo build --release --features candle --bin asr-candle
    ./target/release/asr-candle --threads 8 --dtype f32   # -> data/results/asr-candle-turbo-f32-t8.json
    ./target/release/asr-candle --threads 8 --dtype f16   # ver nota abaixo
    python3 report.py

Notas: `piper-phonemize` (feature `phonemize`, TTS) é GPL-3.0-or-later (espeak-ng) — só para o bench,
nunca no app. Sem ele: `cargo build --release --no-default-features --features sherpa` (ASR+VAD).
Os WAVs do TTS vão para `data/out/<voz>-NN.wav`. Não definir `CARGO_TARGET_DIR` (os build.rs do
sherpa/piper localizam os arquivos baixados via diretório `target`).

`asr-candle` carrega `data/models/whisper-turbo-hf/model.safetensors` (FP16 do HF) com
candle-transformers 0.11 (greedy, t=0, sem timestamps, `<|pt|>` + transcribe forçados; tokens especiais
suprimidos; sem a lista de non-speech tokens do OpenAI). `--threads N` define RAYON_NUM_THREADS e
CANDLE_NUM_THREADS. O 1º build com a feature baixa/compila candle, gemm e onig (precisa de compilador C).
f16/bf16 não funcionam no whisper do candle-transformers 0.11.0 em CPU: `sinusoids` e a máscara causal são sempre F32 (erro: `dtype mismatch in add, lhs: F16, rhs: F32`); só f32 foi medido.
