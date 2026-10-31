# Benchmark de áudio local (Etapa 1)

**Status:** medido em 30/10/2026. A recomendação da seção 8 está **pendente de aprovação do usuário**. Os dados brutos e o script de reprodução ficam em `tools/audio-bench/`; nenhum código do aplicativo foi alterado por esta etapa.

## 1. Objetivo e contexto

Hoje a fala (STT, VAD e TTS) roda em um worker Python persistente, conforme [MODELOS.md](MODELOS.md). A migração planejada troca esse worker por motores Rust **in-process**:

- **STT:** Whisper.
- **VAD:** Silero.
- **TTS:** Piper pt-BR.

Esta etapa mede, fora do aplicativo, candidatos de runtime e de modelo em CPU, para decidir qual motor adotar. A pergunta principal é qual combinação entrega transcrição precisa em pt-BR com latência e RAM aceitáveis, sem depender de Python. Metas gerais de experiência estão em [PLANO.md](PLANO.md).

Candidatos comparados:

- **sherpa-onnx** (runtime ONNX Runtime embutido): Whisper `turbo` e `small`, ambos int8, mais Silero VAD v5 e Piper.
- **candle** (Rust puro): Whisper large-v3-turbo em f32.

## 2. Ambiente

| Item | Valor |
| --- | --- |
| SO | Arch Linux |
| CPU | Intel i7-11800H, 8 núcleos / 16 threads, AVX-512 VNNI |
| RAM | 32 GB |
| GPU | GTX 1650 (**não usada**; todo o benchmark é só CPU) |
| rustc | 1.97.1 |
| sherpa-onnx (crate) | 2.0.1, link estático, ONNX Runtime embutido |
| piper-phonemize (crate) | 1.5.9 |
| candle | 0.11.0 |
| Data da medição | 30/10/2026 |

## 3. Metodologia

**Corpus.** 12 clipes do Common Voice 17 em português, split `test` (`fixie-ai/common_voice_17_0`), com 3,6 a 6,5 s cada, convertidos para 16 kHz mono PCM s16le. O FLEURS foi descartado porque a API de rows do Hugging Face falhou sem `pyarrow`.

**Modelos e artefatos.** Os modelos são baixados por `tools/audio-bench/fetch.py` com revisões (commit) fixadas e SHA-256 registrados em `data/manifest.json`. Fixações explícitas no script:

- Silero VAD: `snakers4/silero-vad` v5.0 (commit `5cd2ba54`).
- Banco de filtros mel do candle: commit `2feb0b054f96e4c4c87f01b243e749896c94f8c7`.

**Métricas.**

- Latência por clipe: p50 e p95.
- RTF médio (tempo de processamento ÷ duração do áudio).
- RSS de pico (VmHWM, uma execução por processo).
- `load_ms`: tempo de carga do modelo.
- WER em nível de corpus.

**Normalização do WER.** NFC, minúsculas, pontuação removida (hífen vira espaço) e espaços colapsados. Há também uma variante sem acento. Números por extenso e dígitos **não** são normalizados entre si: "4" contra "quatro" conta como erro.

**Ressalvas.**

- O Whisper processa uma janela fixa de 30 s. Em clipes curtos o RTF fica pessimista, porque o custo é quase constante por chamada.
- O corpus é pequeno (12 clipes). O WER é indicativo, não estatisticamente forte.

### Como reproduzir

```bash
python3 tools/audio-bench/fetch.py          # baixa tudo (~2 GB), idempotente e retomável
cd tools/audio-bench
cargo build --release                        # 1º build baixa libs pré-compiladas do sherpa-onnx e piper-phonemize
./target/release/asr-sherpa --model turbo --threads 8 --corpus data/corpus --out data/results/asr-sherpa-turbo-t8.json
./target/release/asr-sherpa --model turbo --threads 4 --corpus data/corpus --out data/results/asr-sherpa-turbo-t4.json
./target/release/asr-sherpa --model small --threads 8 --corpus data/corpus --out data/results/asr-sherpa-small-t8.json
./target/release/asr-sherpa --model small --threads 4 --corpus data/corpus --out data/results/asr-sherpa-small-t4.json
./target/release/vad-sherpa --corpus data/corpus
./target/release/tts-sherpa --voice cadu --threads 4 --out-dir data/out
./target/release/tts-sherpa --voice faber --threads 4 --out-dir data/out

# candle (feature opcional, CPU puro, sem MKL)
cargo build --release --features candle --bin asr-candle
./target/release/asr-candle --threads 8 --dtype f32

python3 report.py                            # gera data/results/summary.md
```

Cada binário roda em processo separado e sem outra carga pesada em paralelo. Não definir `CARGO_TARGET_DIR`: os `build.rs` do sherpa e do piper localizam os arquivos baixados pelo diretório `target`. O crate é independente do workspace raiz. Detalhes em `tools/audio-bench/README.md`.

## 4. Resultados de ASR (pt-BR)

| motor | modelo | threads | load ms | p50 ms | p95 ms | RTF médio | RSS MB | WER |
| --- | --- | --- | --- | --- | --- | --- | --- | --- |
| candle | turbo (f32) | 8 | 2033 | 13354 | 14836 | 2.663 | 5198 | 4.0% |
| sherpa-onnx | small (int8) | 4 | 1178 | 1282 | 1534 | 0.247 | 917 | 23.0% |
| sherpa-onnx | small (int8) | 8 | 1333 | 1424 | 1751 | 0.277 | 921 | 23.0% |
| sherpa-onnx | turbo (int8) | 4 | 1548 | 1986 | 2205 | 0.383 | 1816 | 4.0% |
| sherpa-onnx | turbo (int8) | 8 | 1815 | 1583 | 1843 | 0.307 | 1772 | 4.0% |

Sem acento, o WER do `small` cai para 22,0% e o dos demais permanece em 4,0%.

Notas:

- **candle em f16 falha na CPU** (`dtype mismatch in add, lhs: F16, rhs: F32`): o `sinusoids` e a máscara causal do Whisper no candle-transformers 0.11.0 são sempre F32. Só f32 foi medido.
- O candle faz cache apenas do KV de cross-attention.
- **Qualidade qualitativa.** O `small` erra palavras de conteúdo: "embaixada" virou "um baixado", "estômago" virou "estoma" e "Várias" virou "Bárias". O `turbo` erra só nomes raros (clipe 02, "zigurates", transcrito como "gurates").
- `turbo` int8 (sherpa) e `turbo` f32 (candle) têm a mesma WER (4,0%). A quantização int8 não degradou a precisão neste corpus.
- Com 4 threads, o `turbo` fica mais lento no p50 (1986 ms contra 1583 ms com 8). O `small` é o oposto, com p50 menor em 4 threads (1282 ms contra 1424 ms em 8).

## 5. sherpa-onnx contra candle (Whisper turbo)

| Critério | sherpa-onnx (int8, t8) | candle (f32, t8) |
| --- | --- | --- |
| p50 | 1583 ms | 13354 ms |
| RAM de pico | 1772 MB | 5198 MB |
| WER | 4.0% | 4.0% |
| load | 1815 ms | 2033 ms |

- O sherpa-onnx é **~8,4x mais rápido** no p50 e usa **~2,9x menos RAM**, com a mesma WER.
- O candle só ganharia em "Rust puro", sem binário nativo. Isso não compensa a diferença de desempenho.
- O sherpa-onnx também oferece VAD e TTS no mesmo runtime, o que evita carregar dois motores de inferência.

## 6. VAD (Silero v5 via sherpa-onnx)

Modelo: `snakers4/silero-vad` v5.0 (commit `5cd2ba54`), `silero_vad.onnx`.

| Métrica | Valor |
| --- | --- |
| Custo por frame de 512 amostras (32 ms a 16 kHz) | 109,9 µs |
| Uso de CPU em tempo real | ~0,3% de um núcleo |
| Segmentos detectados em silêncio | 0 |
| Segmentos nos 12 clipes de fala | 1 por clipe, em todos |
| RSS de pico | 54 MB |

## 7. TTS (Piper pt_BR via sherpa-onnx + piper-phonemize)

| voz | load ms | sample rate (Hz) | RTF médio | RSS MB |
| --- | --- | --- | --- | --- |
| cadu | 659 | 22050 | 0.046 | 346 |
| faber | 920 | 22050 | 0.038 | 313 |

Ambas rodaram com 4 threads. Na frase mais longa (188 caracteres):

- **cadu:** 472 ms de síntese para 10,7 s de áudio.
- **faber:** 362 ms de síntese para 9,6 s de áudio.

A fonemização leva menos de 1 ms por frase. Os WAVs gerados ficam em `tools/audio-bench/data/out/*.wav` (nome `<voz>-NN.wav`) e a avaliação de naturalidade é auditiva, feita pelo usuário.

**Implicação de licença.** O `piper-phonemize` embute o espeak-ng e é **GPL-3.0-or-later**. O usuário já decidiu adotá-lo mesmo assim (Piper Cadu/Faber via sherpa-onnx + `piper-phonemize`). Isso implica cumprir a GPL-3 na distribuição do aplicativo: código-fonte disponível e aviso de licença no pacote `.deb`. Esse ponto será tratado na Etapa 4. No benchmark o fonemizador fica atrás da feature `phonemize`. Sem ela, o TTS não funciona, mas ASR e VAD continuam funcionando (`cargo build --release --no-default-features --features sherpa`). As vozes têm licenças próprias nos respectivos `MODEL_CARD`.

## 8. Recomendação (pendente de aprovação do usuário)

- **Motor:** sherpa-onnx para ASR, VAD e TTS, em processo único.
- **ASR padrão:** Whisper `turbo` int8 com 8 threads (p95 de ~1,8 s, WER de 4%).
- **Modo leve opcional:** Whisper `small` int8 com 4 threads.
  - RAM de ~0,9 GB e p50 cerca de 20% menor (1282 contra 1583 ms).
  - WER de 23%. A UI deve avisar claramente que a precisão cai.
- **Voz padrão de TTS:** deixar o usuário escolher entre Cadu e Faber. Ambas têm RTF < 0,05 e a escolha é subjetiva (naturalidade).
- **RAM residente estimada** com `turbo` + VAD + uma voz: **≈ 2,1 GB** (1772 + 54 + 346 = 2172 MB). É uma soma de picos, então é um limite superior aproximado.
- **Orçamento de latência.** A latência de ~1,6 s por transcrição é dominada pelo padding da janela fixa de 30 s do Whisper, não pela duração da fala. Otimizações futuras (por exemplo, janelas menores ou outros modelos) estão fora do escopo desta etapa.

## 9. Próximos passos

- **Etapa 2:** implementar o `LocalSpeech` nativo em `support-runtime`, sob a feature `local-speech`.
- **Etapa 3:** integrar o `LocalSpeech` ao aplicativo e às seleções de modelo e voz na UI.
- **Etapa 4:** limpeza, removendo o worker Python e seus artefatos.
