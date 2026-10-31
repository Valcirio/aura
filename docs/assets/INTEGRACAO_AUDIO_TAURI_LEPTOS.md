# Integração de Modelos de Áudio IA Locais em Projeto Tauri + Leptos Existente

> Guia de implementação passo a passo para adicionar ASR (Whisper ONNX) e TTS (Kokoro / Piper PT-BR) a um projeto Tauri 2 + Leptos **já existente**, usando o crate `sherpa-onnx` como runtime único.
>
> **Complementa:** `guia_audio_tauri_leptos.pdf` (referência técnica completa).
>
> **Stack alvo:** Rust · Tauri 2.12+ · Leptos · sherpa-onnx 2.0.1 · piper-phonemize 1.5.9
>
> **Escopo:** ASR e TTS 100% locais, PT-BR, CPU 8 GB RAM (sem GPU garantida).

---

## Sumário

- [1. Pré-requisitos e verificação de compatibilidade](#1-pré-requisitos-e-verificação-de-compatibilidade)
- [2. Backup e preparação do projeto](#2-backup-e-preparação-do-projeto)
- [3. Adicionar dependências Rust no `src-tauri`](#3-adicionar-dependências-rust-no-src-tauri)
- [4. Adicionar `tauri-sys` no frontend Leptos](#4-adicionar-tauri-sys-no-frontend-leptos)
- [5. Criar estrutura de diretórios para modelos](#5-criar-estrutura-de-diretórios-para-modelos)
- [6. Baixar modelos ONNX (scripts)](#6-baixar-modelos-onnx-scripts)
- [7. Implementar módulos Rust de engine](#7-implementar-módulos-rust-de-engine)
- [8. Implementar comandos Tauri no `lib.rs`](#8-implementar-comandos-tauri-no-librs)
- [9. Configurar `tauri.conf.json` (resources + capabilities)](#9-configurar-tauriconfjson-resources--capabilities)
- [10. Implementar componentes Leptos no frontend](#10-implementar-componentes-leptos-no-frontend)
- [11. Inicializar engines no startup do app](#11-inicializar-engines-no-startup-do-app)
- [12. Build, teste e empacotamento](#12-build-teste-e-empacotamento)
- [13. Troubleshooting comum](#13-troubleshooting-comum)
- [14. Checklist final de entrega](#14-checklist-final-de-entrega)

---

## 1. Pré-requisitos e verificação de compatibilidade

### 1.1 Antes de começar, verifique no projeto existente

```bash
# Na raiz do projeto (onde está o Cargo.toml do workspace, se houver)
cd src-tauri

# Versão do Tauri — precisa ser 2.x (idealmente 2.12+)
grep '^tauri = ' Cargo.toml
# Esperado: tauri = { version = "2.12.5", ... }   ou superior 2.x

# Versão do Rust
rustc --version    # preciso >= 1.78

# Node do frontend
node --version     # preciso >= 20

# Estrutura esperada (confirme)
ls src/            # deve conter lib.rs ou main.rs
ls ../src/         # deve conter o frontend Leptos (*.rs + index.html)
ls tauri.conf.json # deve existir
```

### 1.2 Pré-requisitos do sistema operacional

| OS | Comando de instalação |
|----|----------------------|
| **Linux (Debian/Ubuntu)** | `sudo apt install build-essential libstdc++-dev libgomp1 pkg-config libssl-dev` |
| **macOS** | `xcode-select --install` (Xcode CLT) |
| **Windows** | MSVC Build Tools 2022 + Windows SDK (via Visual Studio Installer) |

> ⚠️ **Linux:** o `libgomp1` (runtime OpenMP) é exigido pelo sherpa-onnx em algumas distros. Sem ele, o build compila mas o runtime quebra com `error while loading shared libraries: libgomp.so.1`.

### 1.3 Incompatibilidades conhecidas a verificar

- [ ] **Tauri 1.x:** se o projeto ainda usa Tauri 1, faça o upgrade para 2.x **antes** de prosseguir (a API de `capabilities` e `tauri-plugin-shell` mudou). Veja o [guia de migração oficial](https://v2.tauri.app/start/migrate/from-tauri-1/).
- [ ] **Conflito de `tokio`:** se o projeto já usa `tokio` com features restritas, garanta `features = ["full"]` ou pelo menos `["rt-multi-thread", "macros", "sync", "fs"]`.
- [ ] **`reqwest` já presente:** a versão deve ser 0.12+; versões 0.11 têm API diferente.
- [ ] **`serde` já presente:** garanta `features = ["derive"]`.

---

## 2. Backup e preparação do projeto

### 2.1 Commit de segurança

```bash
cd /caminho/do/seu/projeto
git status                  # tudo commitado?
git checkout -b feat/audio-ia-locais
```

### 2.2 Sanity check do estado atual

```bash
# Roda o app como está — deve abrir sem erro
cargo tauri dev
# Se algo já quebra, NÃO continue. Corrija o estado atual primeiro.
```

---

## 3. Adicionar dependências Rust no `src-tauri`

### 3.1 Via `cargo add` (recomendado — resolve versões automaticamente)

```bash
cd src-tauri

# Dependências principais
cargo add sherpa-onnx@2.0.1
cargo add piper-phonemize@1.5.9          # obrigatório para vozes Piper
cargo add tauri-plugin-shell@2
cargo add hound@3.5                       # leitura/escrita de WAV
cargo add anyhow@1                        # tratamento de erros

# Se ainda não tem estes no projeto:
cargo add serde --features derive
cargo add serde_json
cargo add tokio --features full
cargo add reqwest --features stream       # apenas se for usar download on first run
```

### 3.2 O que deve ficar no `src-tauri/Cargo.toml`

Após os `cargo add`, confira se as linhas abaixo estão presentes em `[dependencies]`:

```toml
[dependencies]
tauri = { version = "2.12.5", features = [] }
tauri-plugin-shell = "2.0"
serde = { version = "1", features = ["derive"] }
serde_json = "1"
tokio = { version = "1", features = ["full"] }
sherpa-onnx = "2.0.1"
piper-phonemize = "1.5.9"
hound = "3.5"
anyhow = "1"
reqwest = { version = "0.12", features = ["stream"], optional = true }

[features]
default = ["custom-protocol"]
download-models = ["dep:reqwest"]   # feature gate para download on first run
```

> 💡 **Dica:** se o projeto já tinha `reqwest` 0.11, faça `cargo update -p reqwest` após o `cargo add` para forçar o bump. Quebras de API entre 0.11 e 0.12 são pequenas mas existem.

### 3.3 Verificar que compila (ainda sem uso)

```bash
cd src-tauri
cargo check
# Esperado: warning unused imports (ok), nenhum error
```

Se aparecer `error: failed to run custom build command for 'sherpa-onnx-sys'`, faltam dependências C++ — volte ao [item 1.2](#12-pré-requisitos-do-sistema-operacional).

---

## 4. Adicionar `tauri-sys` no frontend Leptos

O `tauri-sys` é o crate que expõe `invoke()` e `listen()` ao código WASM do Leptos.

### 4.1 No `Cargo.toml` do frontend (não do `src-tauri`)

```toml
# Geralmente em: <raiz>/Cargo.toml  ou  <raiz>/src/Cargo.toml
[dependencies]
leptos = { version = "0.7", features = ["csr"] }
leptos_meta = "0.7"
leptos_router = "0.7"
serde = { version = "1", features = ["derive"] }
serde_json = "1"
tauri-sys = { version = "5", features = ["all"] }   # ou ["invoke", "event"] para enxugar
wasm-bindgen = "0.2"
log = "0.4"
```

### 4.2 Verificar versão do `tauri-sys`

```bash
# Versão 5.x é compatível com Tauri 2.x
# Versões 3.x ou anteriores são para Tauri 1.x — NÃO usar
cargo search tauri-sys | head -1
```

---

## 5. Criar estrutura de diretórios para modelos

### 5.1 Estrutura alvo

```
src-tauri/
└── resources/
    └── models/
        ├── asr/
        │   └── whisper-small/         # encoder.onnx, decoder.onnx, tokens.txt
        └── tts/
            ├── kokoro-v1_0/            # model.int8.onnx, voices.bin, tokens.txt, lexicon.txt
            ├── piper-pt-BR/            # pt_BR-faber-high.onnx, .onnx.json
            └── espeak-ng-data/         # embutido via piper-phonemize (não precisa criar manual)
```

### 5.2 Criar os diretórios

```bash
cd src-tauri
mkdir -p resources/models/asr/whisper-small
mkdir -p resources/models/tts/kokoro-v1_0
mkdir -p resources/models/tts/piper-pt-BR
```

---

## 6. Baixar modelos ONNX (scripts)

### 6.1 Script único para baixar todos os modelos

Crie `src-tauri/scripts/download-models.sh`:

```bash
#!/usr/bin/env bash
# Baixa todos os modelos necessários para resources/models/
set -euo pipefail

MODELS_DIR="$(dirname "$0")/../resources/models"
mkdir -p "$MODELS_DIR"/{asr/whisper-small,tts/kokoro-v1_0,tts/piper-pt-BR}

echo "==> Baixando Whisper small (ASR, ~480 MB)..."
curl -L https://github.com/k2-fsa/sherpa-onnx/releases/download/asr-models/sherpa-onnx-whisper-small.tar.bz2 \
  | tar -xj --strip-components=1 -C "$MODELS_DIR/asr/whisper-small"

echo "==> Baixando Kokoro multi-lang v1_0 int8 (TTS, ~80 MB)..."
curl -L https://github.com/k2-fsa/sherpa-onnx/releases/download/tts-models/kokoro-multi-lang-v1_0-int8.tar.bz2 \
  | tar -xj --strip-components=1 -C "$MODELS_DIR/tts/kokoro-v1_0"

echo "==> Baixando Piper PT-BR faber-high (TTS alternativo, ~88 MB)..."
curl -L -o "$MODELS_DIR/tts/piper-pt-BR/pt_BR-faber-high.onnx" \
  https://huggingface.co/csukuangfj/vits-piper-pt_BR-faber-high/resolve/main/pt_BR-faber-high.onnx
curl -L -o "$MODELS_DIR/tts/piper-pt-BR/pt_BR-faber-high.onnx.json" \
  https://huggingface.co/csukuangfj/vits-piper-pt_BR-faber-high/resolve/main/pt_BR-faber-high.onnx.json

echo "==> Concluído. Tamanho total:"
du -sh "$MODELS_DIR"
```

### 6.2 Executar o script

```bash
cd src-tauri
chmod +x scripts/download-models.sh
./scripts/download-models.sh

# Verificar
ls resources/models/asr/whisper-small/
# Esperado: encoder.onnx  decoder.onnx  tokens.txt  ...

ls resources/models/tts/kokoro-v1_0/
# Esperado: model.int8.onnx  voices.bin  tokens.txt  lexicon.txt  ...
```

### 6.3 Adicionar o diretório de modelos ao `.gitignore`

Os modelos são grandes (~700 MB no total) e **não devem ser commitados**:

```gitignore
# Em src-tauri/.gitignore (ou no .gitignore da raiz)
resources/models/
```

> ⚠️ **Importante:** se o CI/CD for construir o bundle, ele precisa executar `download-models.sh` antes do `cargo tauri build`. Documente isso no `README.md` do projeto.

### 6.4 Alternativa: modelos opcionais (skip se só Kokoro for suficiente)

Se você não quiser Piper (e sim só Kokoro), **pode pular** o download do Piper PT-BR. Mas precisa **também** remover o `VitsModelConfig` do `tts_engine.rs` no passo 7. Caso contrário, o build quebra.

---

## 7. Implementar módulos Rust de engine

Crie dois arquivos em `src-tauri/src/`:

### 7.1 `src-tauri/src/asr_engine.rs`

```rust
use sherpa_onnx::{
    OfflineRecognizer, OfflineRecognizerConfig, WhisperModelConfig,
};

/// Constrói o recognizer ASR apontando para os arquivos Whisper ONNX empacotados.
///
/// `models_dir` deve ser o path absoluto para `resources/models` resolvido via
/// `app.path().resolve_resource("models")`.
pub fn build_recognizer(models_dir: &str) -> Result<OfflineRecognizer, String> {
    let cfg = OfflineRecognizerConfig {
        whisper: WhisperModelConfig {
            encoder: format!("{models_dir}/asr/whisper-small/encoder.onnx"),
            decoder: format!("{models_dir}/asr/whisper-small/decoder.onnx"),
            tokens:  format!("{models_dir}/asr/whisper-small/tokens.txt"),
            language: "pt".into(),
            task: "transcribe".into(),
            ..Default::default()
        },
        ..Default::default()
    };
    OfflineRecognizer::new(cfg)
        .map_err(|e| format!("sherpa-onnx ASR init: {e}"))
}
```

### 7.2 `src-tauri/src/tts_engine.rs`

```rust
use sherpa_onnx::{
    KokoroTtsModelConfig, OfflineTts, OfflineTtsConfig, OfflineTtsModelConfig,
    VitsModelConfig,
};

/// Constrói o TTS principal (Kokoro multi-lang v1_0 — voz PT-BR pf_dora).
pub fn build_tts_kokoro(models_dir: &str) -> Result<OfflineTts, String> {
    let kokoro_dir = format!("{models_dir}/tts/kokoro-v1_0");
    let cfg = OfflineTtsConfig {
        model: OfflineTtsModelConfig {
            kokoro: KokoroTtsModelConfig {
                model:    format!("{kokoro_dir}/model.int8.onnx"),
                voices:   format!("{kokoro_dir}/voices.bin"),
                tokens:   format!("{kokoro_dir}/tokens.txt"),
                data_dir: kokoro_dir,           // diretório contendo lexicon.txt
                ..Default::default()
            },
            num_threads: 2,
            ..Default::default()
        },
        ..Default::default()
    };
    OfflineTts::new(cfg).map_err(|e| format!("sherpa-onnx TTS init: {e}"))
}

/// Constrói o TTS alternativo (Piper PT-BR faber-high) — exige piper-phonemize.
pub fn build_tts_piper(models_dir: &str) -> Result<OfflineTts, String> {
    let cfg = OfflineTtsConfig {
        model: OfflineTtsModelConfig {
            vits: VitsModelConfig {
                model:    format!("{models_dir}/tts/piper-pt-BR/pt_BR-faber-high.onnx"),
                tokens:   format!("{models_dir}/tts/piper-pt-BR/tokens.txt"),
                data_dir: format!("{models_dir}/tts/espeak-ng-data"),
                ..Default::default()
            },
            num_threads: 2,
            ..Default::default()
        },
        ..Default::default()
    };
    OfflineTts::new(cfg).map_err(|e| format!("piper TTS init: {e}"))
}
```

> 💡 **Escolha:** no passo 8 você registrará no estado do app **ou** `build_tts_kokoro` **ou** `build_tts_piper`. Não vale a pena manter ambos carregados simultaneamente (consome RAM desnecessária).

---

## 8. Implementar comandos Tauri no `lib.rs`

### 8.1 Substituir/editar `src-tauri/src/lib.rs`

Ajuste ao seu projeto existente — provavelmente você já tem um `Builder::default()` com plugins. **Adicione** o que está marcado com `// NOVO`:

```rust
mod asr_engine;   // NOVO
mod tts_engine;   // NOVO

use std::sync::Mutex;
use sherpa_onnx::{OfflineRecognizer, OfflineTts};
use tauri::Manager;   // NOVO — necessário para app.path(), State, etc.

// NOVO — estado compartilhado entre comandos
pub struct AppState {
    pub asr: Mutex<Option<OfflineRecognizer>>,
    pub tts: Mutex<Option<OfflineTts>>,
}

// NOVO — comando de inicialização (chame do frontend no startup)
#[tauri::command]
async fn init_engines(
    state: tauri::State<'_, AppState>,
    app: tauri::AppHandle,
) -> Result<(), String> {
    let resource_path = app
        .path()
        .resolve_resource("models")
        .map_err(|e| format!("resolve_resource: {e}"))?;
    let models_dir = resource_path.to_string_lossy().to_string();

    let asr = asr_engine::build_recognizer(&models_dir)?;
    let tts = tts_engine::build_tts_kokoro(&models_dir)?;

    *state.asr.lock().unwrap() = Some(asr);
    *state.tts.lock().unwrap() = Some(tts);
    Ok(())
}

// NOVO — comando de transcrição ASR
#[tauri::command]
async fn transcribe_audio(
    state: tauri::State<'_, AppState>,
    path: String,
) -> Result<String, String> {
    let mut reader = hound::WavReader::open(&path)
        .map_err(|e| format!("wav open: {e}"))?;
    let spec = reader.spec();
    if spec.sample_rate != 16000 || spec.channels != 1 {
        return Err("WAV deve ser 16 kHz mono PCM int16".into());
    }
    let samples: Vec<f32> = reader
        .samples::<i16>()
        .filter_map(|s| s.ok())
        .map(|s| s as f32 / 32768.0)
        .collect();

    let guard = state.asr.lock().unwrap();
    let recognizer = guard
        .as_ref()
        .ok_or("ASR não inicializado — chame init_engines primeiro")?;

    let mut stream = recognizer
        .create_stream()
        .map_err(|e| e.to_string())?;
    stream.accept_waveform(16000, &samples);
    recognizer.decode_stream(&mut stream);

    Ok(stream.result().text.to_string())
}

// NOVO — comando de síntese TTS com streaming de tokens
#[tauri::command]
async fn synthesize_speech(
    state: tauri::State<'_, AppState>,
    app: tauri::AppHandle,
    text: String,
    voice: String,
    out_path: String,
) -> Result<String, String> {
    let guard = state.tts.lock().unwrap();
    let tts = guard
        .as_ref()
        .ok_or("TTS não inicializado — chame init_engines primeiro")?;

    let voice_id: i32 = voice.parse().unwrap_or(0);
    let app_handle = app.clone();

    let audio = tts
        .generate(&text, voice_id, 1.0, move |tokens: &[i32], _progress: i32| {
            let _ = app_handle.emit("tts-token", tokens.to_vec());
        })
        .map_err(|e| format!("tts generate: {e}"))?;

    let spec = hound::WavSpec {
        sample_rate: 16000,
        channels: 1,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };
    let mut writer = hound::WavWriter::create(&out_path, spec)
        .map_err(|e| format!("wav write: {e}"))?;
    for s in &audio.samples {
        let v = (s.clamp(-1.0, 1.0) * 32767.0) as i16;
        writer.write_sample(v).map_err(|e| e.to_string())?;
    }
    writer.finalize().map_err(|e| e.to_string())?;

    Ok(out_path)
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_shell::init())           // NOVO
        .manage(AppState {                            // NOVO
            asr: Mutex::new(None),
            tts: Mutex::new(None),
        })
        .invoke_handler(tauri::generate_handler![    // NOVO — adicione os 3 comandos
            init_engines,
            transcribe_audio,
            synthesize_speech,
            // ... seus comandos existentes ...
        ])
        .run(tauri::generate_context![])
        .expect("erro ao rodar tauri");
}
```

### 8.2 Verificar que compila

```bash
cd src-tauri
cargo check
```

Se aparecer `unresolved import: sherpa_onnx::OfflineTtsConfig`, o `sherpa-onnx` pode estar em versão diferente da 2.0.1 — confira `Cargo.lock`.

---

## 9. Configurar `tauri.conf.json` (resources + capabilities)

### 9.1 Adicionar `resources` no bloco `bundle`

Edite `src-tauri/tauri.conf.json`. **Localize** o bloco `"bundle"` e adicione a chave `"resources"` (não apague o que já existe):

```json
{
  "bundle": {
    "active": true,
    "targets": ["msi", "deb", "appimage", "dmg"],
    "resources": [
      "resources/models/asr/whisper-small/*",
      "resources/models/tts/kokoro-v1_0/*",
      "resources/models/tts/piper-pt-BR/*"
    ],
    "icon": ["icons/icon.png"]
  }
}
```

> ⚠️ Se você está usando a estratégia de **download on first run** em vez de bundling, **não** adicione `resources` — apenas garanta que o comando `init_engines` baixe os modelos antes de carregar os engines. Veja o capítulo 7.2 do PDF para o código do downloader.

### 9.2 Adicionar permissões no `capabilities/default.json`

Edite `src-tauri/capabilities/default.json` (crie se não existir):

```json
{
  "$schema": "../gen/schemas/desktop-schema.json",
  "identifier": "default",
  "description": "Permissões do app desktop",
  "windows": ["main"],
  "permissions": [
    "core:default",
    "core:event:default",
    "shell:allow-execute",
    "shell:allow-spawn"
  ]
}
```

> ⚠️ **Sem `"core:event:default"`** o `app.emit("tts-token", ...)` do backend é silenciosamente descartado e o frontend não recebe os eventos de streaming de tokens.

---

## 10. Implementar componentes Leptos no frontend

### 10.1 Criar `src/components/asr_panel.rs`

```rust
use leptos::*;
use serde::{Deserialize, Serialize};
use tauri_sys::invoke;

#[derive(Serialize, Deserialize)]
struct TranscribeArgs<'a> {
    path: &'a str,
}

#[component]
pub fn AsrPanel() -> impl IntoView {
    let (transcript, set_transcript) = create_signal(String::new());
    let (busy, set_busy) = create_signal(false);
    let (error, set_error) = create_signal(String::new());

    let on_transcribe = create_action(move |_| {
        let path = "/tmp/audio.wav".to_string(); // ajuste conforme sua fonte de áudio
        set_busy.set(true);
        set_error.set(String::new());
        async move {
            let args = TranscribeArgs { path: &path };
            match invoke::<String>("transcribe_audio", &args).await {
                Ok(text) => set_transcript.set(text),
                Err(e) => set_error.set(format!("{e:?}")),
            }
            set_busy.set(false);
        }
    });

    view! {
        <div class="asr-panel">
            <button on:click=move |_| on_transcribe.dispatch(())>
                "Transcrever áudio"
            </button>
            <p class="status">
                {move || if busy.get() { "Processando..." } else { "Pronto" }}
            </p>
            {move || {
                let err = error.get();
                (!err.is_empty()).then(|| view! { <p class="error">{err}</p> })
            }}
            <p class="transcript">{move || transcript.get()}</p>
        </div>
    }
}
```

### 10.2 Criar `src/components/tts_panel.rs`

```rust
use leptos::*;
use serde::Deserialize;
use tauri_sys::event::listen;
use tauri_sys::invoke;

#[derive(Deserialize)]
struct TokenEvent {
    payload: Vec<i32>,
}

#[component]
pub fn TtsPanel() -> impl IntoView {
    let (tokens, set_tokens) = create_signal(Vec::<i32>::new());
    let (audio_path, set_audio_path) = create_signal(String::new());

    // Ouvir eventos de streaming do TTS
    create_effect(move |_| {
        let setter = set_tokens;
        spawn_local(async move {
            let mut rx = listen::<TokenEvent>("tts-token").await.unwrap();
            while let Some(ev) = rx.recv().await {
                let mut current = setter.get_untracked();
                current.extend(ev.payload);
                setter.set(current);
            }
        });
    });

    let on_synthesize = create_action(move |_| {
        set_tokens.set(Vec::new());
        async move {
            #[derive(serde::Serialize)]
            struct SynthArgs<'a> {
                text: &'a str,
                voice: &'a str,
                out_path: &'a str,
            }
            let args = SynthArgs {
                text: "Olá, mundo. Este é um teste de síntese local.",
                voice: "0",
                out_path: "/tmp/tts-out.wav",
            };
            let result: Result<String, _> = invoke("synthesize_speech", &args).await;
            if let Ok(p) = result {
                set_audio_path.set(p);
            }
        }
    });

    view! {
        <div class="tts-panel">
            <button on:click=move |_| on_synthesize.dispatch(())>
                "Sintetizar fala"
            </button>
            <p class="token-count">
                {move || format!("Tokens gerados: {}", tokens.get().len())}
            </p>
            {move || {
                let p = audio_path.get();
                (!p.is_empty()).then(|| view! { <p class="path">"Áudio salvo em: "{p}</p> })
            }}
        </div>
    }
}
```

### 10.3 Adicionar os componentes ao `App.rs`

```rust
// Em src/App.rs (ou equivalente no seu projeto)
mod components; // se usar módulo
use components::asr_panel::AsrPanel;
use components::tts_panel::TtsPanel;

#[component]
fn App() -> impl IntoView {
    view! {
        <main>
            {/* ... seu conteúdo existente ... */}
            <AsrPanel />
            <TtsPanel />
        </main>
    }
}
```

---

## 11. Inicializar engines no startup do app

### 11.1 Chamar `init_engines` no `on_mount` da raiz

```rust
// Em src/App.rs
use tauri_sys::invoke;

#[component]
fn App() -> impl IntoView {
    on_mount(|| {
        spawn_local(async {
            match invoke::<()>("init_engines", &()).await {
                Ok(_) => log::info!("Engines ASR/TTS inicializados"),
                Err(e) => log::error!("Falha ao inicializar engines: {e:?}"),
            }
        });
    });

    view! { /* ... */ }
}
```

> ⚠️ **Sem chamar `init_engines`, qualquer chamada a `transcribe_audio` ou `synthesize_speech` retorna erro `"não inicializado"`.** Esse é o bug #1 mais comum para quem está integrando pela primeira vez.

### 11.2 (Opcional) Tela de loading enquanto inicializa

Os engines levam ~2-5 segundos para carregar na primeira chamada. Considere uma tela de loading reativa:

```rust
let (engines_ready, set_engines_ready) = create_signal(false);

on_mount(|| {
    spawn_local(async {
        let _ = invoke::<()>("init_engines", &()).await;
        set_engines_ready.set(true);
    });
});

view! {
    <main>
        {move || if engines_ready.get() {
            view! { <AsrPanel /><TtsPanel /> }.into_view()
        } else {
            view! { <div>"Carregando modelos de áudio..."</div> }.into_view()
        }}
    </main>
}
```

---

## 12. Build, teste e empacotamento

### 12.1 Teste em modo dev

```bash
# Na raiz do projeto
cargo tauri dev
```

**Fluxo de teste manual recomendado:**

1. App abre sem erro na webview
2. Console do navegador (DevTools) mostra `"Engines ASR/TTS inicializados"`
3. Grave um WAV curto em PT-BR (5-10s) com `ffmpeg -f pulse -i default -ar 16000 -ac 1 -c:a pcm_s16le /tmp/audio.wav`
4. Clique em "Transcrever áudio" — deve mostrar o texto transcrito
5. Clique em "Sintetizar fala" — deve mostrar contagem de tokens subindo + arquivo em `/tmp/tts-out.wav`
6. Toque o WAV gerado: `ffplay /tmp/tts-out.wav` ou `aplay /tmp/tts-out.wav`

### 12.2 Build de produção

```bash
cargo tauri build
# Outputs em src-tauri/target/release/bundle/{msi,deb,appimage,dmg}/
```

### 12.3 Verificar que os modelos foram empacotados

```bash
# Linux DEB
dpkg-deb -c src-tauri/target/release/bundle/deb/*.deb | grep models
# Esperado: várias linhas com usr/lib/.../resources/models/asr/whisper-small/encoder.onnx etc.

# macOS DMG
hdiutil attach src-tauri/target/release/bundle/dmg/*.dmg
ls "/Volumes/Seu App/Seu App.app/Contents/Resources/models/"
hdiutil detach "/Volumes/Seu App"
```

### 12.4 Tamanho final do bundle esperado

| Configuração | Tamanho aproximado |
|-------------|---------------------|
| Whisper small + Kokoro int8 | ~250 MB |
| Whisper small + Kokoro fp16 | ~480 MB |
| Whisper small + Kokoro int8 + Piper | ~340 MB |
| Whisper tiny + Kokoro int8 | ~110 MB |

---

## 13. Troubleshooting comum

| Sintoma | Causa | Solução |
|---------|-------|---------|
| `symbol not found: espeak_ng_*` em runtime | Falta `piper-phonemize` no `Cargo.toml` | `cargo add piper-phonemize@1.5.9` |
| `panicked at sherpa_onnx::recognizer: model not found` | Path hardcoded em vez de `resolve_resource` | Use sempre `app.path().resolve_resource("models")` |
| Áudio roda mas é incompreensível | WAV não está em 16 kHz mono PCM int16 | Reamostrar: `ffmpeg -ar 16000 -ac 1 -c:a pcm_s16le input.wav` |
| Build Linux falha em `-lgomp` | Falta OpenMP runtime | `sudo apt install libgomp1` (Debian/Ubuntu) |
| Invoke retorna `"permission denied"` | Falta `"core:event:default"` em `capabilities/default.json` | Adicione a permissão e rebuild |
| TTS gera áudio truncado | Limite de tokens do Kokoro (~510) | Quebre texto em frases com `text.split('.')` e sintetize cada uma |
| `error: failed to run custom build command for 'sherpa-onnx-sys'` | Falta compilador C++ | Linux: `build-essential` · macOS: Xcode CLT · Windows: MSVC |
| `unresolved import: tauri_sys::event::listen` | `tauri-sys` sem feature `event` | No `Cargo.toml` do frontend: `tauri-sys = { version = "5", features = ["all"] }` |
| Bundle > 500 MB | Modelos fp16 em vez de int8 | Use `model.int8.onnx` no Kokoro (300 → 80 MB) |

---

## 14. Checklist final de entrega

### 14.1 Código

- [ ] `src-tauri/Cargo.toml` tem `sherpa-onnx`, `piper-phonemize`, `hound`, `tauri-plugin-shell`
- [ ] `Cargo.toml` do frontend tem `tauri-sys` v5+ com features `invoke` e `event`
- [ ] `src-tauri/src/asr_engine.rs` criado com `build_recognizer`
- [ ] `src-tauri/src/tts_engine.rs` criado com `build_tts_kokoro` (e/ou `build_tts_piper`)
- [ ] `src-tauri/src/lib.rs` declara `mod asr_engine; mod tts_engine;`
- [ ] `lib.rs` registra `AppState` via `.manage(...)` e os 3 comandos no `invoke_handler`
- [ ] Comando `init_engines` existe e usa `app.path().resolve_resource("models")`

### 14.2 Modelos

- [ ] `src-tauri/resources/models/asr/whisper-small/` contém `encoder.onnx`, `decoder.onnx`, `tokens.txt`
- [ ] `src-tauri/resources/models/tts/kokoro-v1_0/` contém `model.int8.onnx`, `voices.bin`, `tokens.txt`, `lexicon.txt`
- [ ] (Opcional) `src-tauri/resources/models/tts/piper-pt-BR/` contém `pt_BR-faber-high.onnx` e `.json`
- [ ] `src-tauri/.gitignore` exclui `resources/models/`
- [ ] `scripts/download-models.sh` é executável e documentado no `README.md`

### 14.3 Configuração Tauri

- [ ] `tauri.conf.json` tem `"resources"` listando os 2-3 diretórios de modelo
- [ ] `capabilities/default.json` tem `"core:event:default"` e `"shell:allow-execute"`
- [ ] `identifier` do app está preenchido (ex.: `"com.seuapp.audio"`)

### 14.4 Frontend

- [ ] Componente `AsrPanel` criado e importado no `App.rs`
- [ ] Componente `TtsPanel` criado e importado no `App.rs`
- [ ] `on_mount` chama `invoke("init_engines", &())` no startup
- [ ] (Opcional) Tela de loading reativa enquanto engines inicializam

### 14.5 Validação

- [ ] `cargo check` no `src-tauri` sem erros
- [ ] `cargo tauri dev` abre o app sem erro
- [ ] Console do navegador mostra `"Engines ASR/TTS inicializados"`
- [ ] Transcrição de um WAV de teste retorna texto em PT-BR
- [ ] Síntese de um texto curto gera WAV tocável em `/tmp/tts-out.wav`
- [ ] `cargo tauri build` produz instalador com modelos embutidos
- [ ] Instalador testado em máquina limpa (sem Rust instalado) funciona

---

## 15. Próximos passos sugeridos

Após a integração básica funcionando, considere:

1. **VAD (Voice Activity Detector)** — use `VoiceActivityDetector` do sherpa-onnx para detectar automaticamente início/fim da fala no microfone, sem botão de gravar.
2. **OnlineRecognizer (streaming ASR)** — para transcrição em tempo real enquanto o usuário fala, em vez de gravar tudo e depois transcrever.
3. **Múltiplas vozes Kokoro** — a voz `pf_dora` é a PT-BR principal, mas o `voices.bin` contém outras. Liste as disponíveis via `tts.num_speakers()` e ofereça seleção no UI.
4. **Cache de áudio sintetizado** — se o app repete os mesmos textos (ex.: respostas fixas de voz), cacheie os WAVs em `app_data_dir` com hash do texto+voz como chave.
5. **Download on first run** — para reduzir o instalador, baixe modelos na primeira execução via `reqwest` (ver cap. 7.2 do PDF).
6. **CI/CD** — adicione o passo `./src-tauri/scripts/download-models.sh` antes do `cargo tauri build` no GitHub Actions.

---

## Referências rápidas

| Recurso | URL |
|---------|-----|
| sherpa-onnx (crate) | https://crates.io/crates/sherpa-onnx |
| sherpa-onnx (repo) | https://github.com/k2-fsa/sherpa-onnx |
| Exemplos Rust oficiais | https://github.com/k2-fsa/sherpa-onnx/tree/master/rust-api-examples |
| Modelos ASR (Whisper ONNX) | https://github.com/k2-fsa/sherpa-onnx/releases/tag/asr-models |
| Modelos TTS (Kokoro/VITS) | https://github.com/k2-fsa/sherpa-onnx/releases/tag/tts-models |
| piper-phonemize (crate) | https://crates.io/crates/piper-phonemize |
| whisper-rs (alternativa) | https://crates.io/crates/whisper-rs |
| Tauri sidecar docs | https://v2.tauri.app/develop/sidecar/ |
| tauri-plugin-shell | https://v2.tauri.app/plugin/shell/ |
| Leptos docs | https://leptos.dev/ |
| Piper PT-BR no HF | https://huggingface.co/csukuangfj/vits-piper-pt_BR-faber-high |
| Kokoro no HF | https://huggingface.co/hexgrad/Kokoro-82M |
| Guia PDF complementar | `guia_audio_tauri_leptos.pdf` (neste mesmo diretório) |
