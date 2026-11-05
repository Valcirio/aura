# Estrutura do projeto Aura

> Estado em 2026-11-05, após a Fase A da refatoração para agente generalista (ainda não commitada).
> Contratos de comandos e eventos: `CONTRATOS.md`. Visão de arquitetura: `ARQUITETURA.md`.

## Visão geral

```
assistant/
├─ crates/
│  ├─ support-core/      # domínio puro: SQLite, ferramentas, operações, calendário, VAD
│  └─ support-runtime/   # backend: agente, provedor LLM, integrações, voz nativa, modelos
├─ src-tauri/            # casca desktop Tauri 2 (hospeda o runtime em processo)
├─ src/                  # UI Leptos (CSR/wasm)
├─ public/               # bridge.js (ponte Tauri↔wasm), styles.css
├─ workers/              # LEGADO: worker de fala em Python (remoção na Etapa 4)
├─ tools/audio-bench/    # benchmark offline de ASR/TTS/VAD
├─ scripts/              # dev.sh, build-linux.sh, verify-runtime.py, bridge.test.mjs, setup-audio.sh (legado)
└─ docs/
```

Dependências entre as partes:

```
src/ (wasm) ──invoke app_command / app_stream──▶ src-tauri ──▶ support-runtime ──▶ support-core
                                                                  ▲
                                         bin aura-runtime (NDJSON) ┘  (smoke: scripts/verify-runtime.py)
```

Cada crate tem `[workspace]` próprio. O `Cargo.toml` raiz (UI `aura-ui`) exclui `crates/*` e `src-tauri`.

---

## 1. `crates/support-core`

### Função
É a biblioteca de domínio, sem Tauri nem rede. Ela concentra:
- a persistência em SQLite (sessões, mensagens, FTS, configurações, conexões, documentos, turnos, operações);
- o contrato de ferramentas (`ToolSpec`, `Effect`) com validação de argumentos por JSON Schema;
- o ciclo de vida das operações que exigem aprovação;
- a validação de eventos de calendário com fuso e DST;
- utilitários de áudio (segmentador VAD, resampler).

Ela é usada por `support-runtime` e por `src-tauri`.

Dependências: `serde`, `serde_json`, `rusqlite` (bundled), `jiff` (fuso horário) e `uuid`.

### Arquitetura

```
crates/support-core/
├─ Cargo.toml
└─ src/
   ├─ lib.rs           (9)   declara os módulos públicos
   ├─ error.rs         (54)  AppError{code,message,retryable} + Result
   ├─ schema.sql       (28)  sessions, messages, messages_fts (+ triggers); WAL e FKs
   ├─ memory.rs        (178) Memory: open/ephemeral, sessões, mensagens, busca FTS
   ├─ store.rs         (672) migrações v1/v2 + impl Memory: settings, conexões, documentos,
   │                         turnos e ciclo de operações (prepare → claim → finish / cancel / retry / expire / recover)
   ├─ tools.rs         (369) Effect{Read,LocalWrite,ExternalWrite,Destructive,System}, ToolSpec,
   │                         ActionField, validate_schema (subconjunto de JSON Schema, profundidade 5)
   ├─ calendar.rs      (252) EventInput/EventDraft, GOOGLE_CALENDAR, CREATE_EVENT_TOOL,
   │                         Memory::prepare_calendar_event, recusa de horários ambíguos (DST)
   ├─ conversation.rs  (108) máquina de fases de turno (Phase/Conversation)
   ├─ audio.rs         (147) VadSegmenter e Resampler (FIR sinc)
   └─ models.rs        (69)  Capabilities::incompatibilities()
```

- **Banco:**
  - `schema.sql` cria a base.
  - `store::migrate` aplica as migrações:
    - **v1**: `settings`, `connections`, `documents` (+ FTS), `turns`, `operations`;
    - **v2**: colunas genéricas em `operations` (`tool`, `effect`, `summary`, `fields`), com conversão das linhas antigas de evento.
  - Uma versão maior que 2 é recusada.
- **Operações:** uma operação pendente por sessão. Ela expira em 600 s. Criar uma nova cancela a anterior.
- **Testes:** cerca de 42, a maior parte em `tools.rs` (14) e `store.rs` (10).

### O que falta
- **Resquícios de calendário no núcleo genérico:**
  - `store.rs` importa `calendar::EventDraft` para a migração v2;
  - o default `'calendar_create_event'` na coluna `tool`;
  - a coluna `operations.event` guarda o payload genérico;
  - as colunas `service_id`/`account_id` existem, mas o struct usa `integration`.
  - `Memory::prepare_calendar_event` poderia sair do core.
- **Código possivelmente morto:** `conversation.rs` não é usado fora do crate.
- **Fase B:** migração v3 com as tabelas de tarefas, lembretes e notas (notas com FTS, lembretes com `due_at` e estado). O teto `version > 2` precisa subir.
- **Fase B:** a regra "uma operação pendente por sessão, 600 s fixos" limita aprovações em lote.
- **Fase C:** não há modelo de pastas autorizadas nem de validação de caminhos.
- O SQL de `store.rs` está em linhas muito longas, o que dificulta a manutenção.

---

## 2. `crates/support-runtime`

### Função
É o backend completo do Aura, em Rust:
- sessões e memória;
- provedor LLM (Ollama ou API compatível com OpenAI, via SSE);
- o loop único do agente com ferramentas e aprovações;
- as integrações (memória, conhecimento, web, Google Calendar);
- a voz local nativa (Silero VAD, Whisper e Kokoro `pf_dora`, via `sherpa-onnx`/`ort`), captura e reprodução (`cpal`);
- o catálogo e a instalação verificada de modelos;
- os segredos via `secret-tool` (libsecret).

Ele é consumido de duas formas:
- por `src-tauri`, em processo, com `commands::dispatch`;
- pelo binário `aura-runtime`, um harness NDJSON em stdin/stdout usado no smoke `scripts/verify-runtime.py`.

### Arquitetura

```
crates/support-runtime/
├─ Cargo.toml          features: default = [native-audio, local-speech]
│                        native-audio = cpal | local-speech = sherpa-onnx, piper-phonemize, ort
├─ assets/notices/     avisos falados pré-gravados (capture, models, provider, synthesis .wav)
└─ src/
   ├─ lib.rs              (22)   módulos + now()
   ├─ bin/aura-runtime.rs (58)   harness NDJSON {id,command,args} → {id,result|error} / {event}
   ├─ commands.rs         (734)  dispatch(): allowlist fechada; `_` → COMMAND_NOT_ALLOWED
   ├─ runtime.rs          (2992) Runtime: estado central, start/turn, voice_loop, Emitter, Speaker,
   │                             pipeline de fala, confirm/retry/reconcile, speech_prepare; ~1400 linhas de testes
   ├─ agent/
   │  ├─ mod.rs           (752)  answer() → rounds() → stream_round() → run_call()/record()
   │  ├─ prompt.rs        (151)  prompt de sistema genérico (integrações ativas e inativas, políticas)
   │  └─ approvals.rs     (70)   "sim/não" por texto ou voz para a operação pendente
   ├─ integrations/
   │  ├─ mod.rs           (477)  trait Integration + IntegrationRegistry (dispatch com validação de schema)
   │  ├─ builtin.rs       (191)  memory, knowledge, web
   │  └─ calendar.rs      (289)  CalendarIntegration (list/events/free_busy = Read; create_event = ExternalWrite)
   ├─ provider.rs         (536)  chat em streaming + tool_calls (máx. 6 por rodada), models, probe
   ├─ config.rs           (238)  Settings (deny_unknown_fields), ProviderConfig, Paths
   ├─ google.rs           (879)  OAuth loopback + PKCE, CalendarApi, Database/database()
   ├─ knowledge.rs        (221)  documentos semeados/atualizados, extração de HTML, busca Brave
   ├─ http.rs             (81)   cliente reqwest + mapeamento de erros
   ├─ secrets.rs          (93)   SecretStore: cache em RAM + secret-tool
   ├─ models.rs           (1822) catálogo (whisper-turbo/small, silero-vad, kokoro) e instalação atômica com SHA-256
   ├─ local_speech.rs     (1136) LocalSpeech: VAD, ASR e TTS nativos
   ├─ speech.rs           (296)  Pcm, Sentences, SpeechBackend; SpeechWorker (Python, legado)
   └─ audio.rs            (584)  captura, reprodução e dispositivos (cpal), decodificação de WAV, rms
```

**Turno de texto:**
1. `conversation_start{text}` chama `Runtime::start`, que valida o texto, cancela o run anterior, faz `spawn` e devolve o ticket.
2. `turn()` grava a mensagem do usuário e chama `agent::answer`.
3. `answer` segue uma de duas rotas:
   - se houver uma operação pendente, `approvals` trata o "sim/não";
   - caso contrário, monta o prompt, o histórico (20 mensagens dentro do orçamento de contexto) e as ferramentas, e entra em `rounds`.
4. `rounds` funciona assim:
   - são até 8 rodadas com ferramentas, mais uma final sem ferramentas, e no máximo 12 chamadas por turno;
   - rodadas só de leitura rodam 2 chamadas em paralelo; rodadas com escrita rodam em série;
   - uma ferramenta que exige aprovação cria uma `Operation` (`awaiting_confirmation`) e força a rodada final.
5. Os deltas saem como `assistant.delta` e as frases completas vão para o `Speaker` (TTS em paralelo).

**Turno de voz:** cpal → Silero VAD (frames de 512) → Whisper → o mesmo `turn()` → Kokoro → reprodução. O PCM não sai do Rust; a WebView só recebe níveis (`audio.level`).

**Eventos:**
- `run.started`, `state.changed`, `audio.level`, `transcript.final`, `assistant.delta`;
- `tool.started`, `tool.completed`, `sources`, `action.changed`;
- `turn.completed`, `turn.cancelled`, `run.completed`, `error`, `audio.error`, `model.progress`.

**Testes:** 113 com as features padrão (+5 ignorados que exigem modelos de `tools/audio-bench/data`) e 73 com `--no-default-features`.

### O que falta
- **Etapa 4 (Python):**
  - remover `SpeechWorker` (`speech.rs`), `Paths.python`/`AURA_PYTHON` (`config.rs`), o teste de paridade VAD com Python (`local_speech.rs`) e `python:` nos `test_paths`;
  - o formato de pedido de `local_speech.request` ainda imita o protocolo JSON do worker;
  - decidir se `local-speech` vira obrigatória.
- **`runtime.rs` grande demais:**
  - dividir em `runtime/{turn,voice,speech_job,provider}.rs` e mover os testes para arquivos próprios;
  - `confirm`, `retry` e `reconcile` repetem a montagem do `ToolContext`.
- **Resquícios de Calendar:**
  - `Settings.calendar_id`/`google_client_id`/`service_id` (obsoleto);
  - `Runtime.google` como campo de primeira classe;
  - `integration_connect`/`disconnect` e `connectable` fixos em `google_calendar`;
  - comandos `calendar_*` diretos;
  - `Database`/`database()` dentro de `google.rs`;
  - o conhecimento semeado é só do Calendar.
- **Limites do agente:**
  - mais de uma aprovação na mesma rodada dá `ACTION_AMBIGUOUS`, o que atrapalha pedidos como "crie 3 lembretes";
  - estourar o orçamento dá erro em vez de resposta parcial;
  - só uma operação pendente por sessão.
- **Fase B:** integração de tarefas, lembretes e notas, mais um **agendador em segundo plano**, que não existe hoje (ex.: evento `reminder.due` para `src-tauri` notificar).
- **Fase C:** integração de arquivos com raiz autorizada (`Settings` + `ToolContext`), proteção contra path traversal e symlinks (no padrão de `models.rs`), e abrir URL, arquivo ou app com aprovação.
- **Fase D:** comandos e eventos para listar e gerenciar tarefas e notas na UI.
- **Sem testes diretos:** `approvals.rs`, `builtin.rs`, `secrets.rs`, `http.rs`.
- Campos novos em `Settings` exigem `#[serde(default)]` por causa de `deny_unknown_fields`.

---

## 3. `workers/` (legado)

### Função
É o antigo motor de fala em Python. Era um processo persistente lançado pelo runtime (`python -u workers/speech_worker.py --models <dir>`) que conversava por JSON em linhas via stdin e stdout, com o PCM em base64.
- **Comandos:** `status`, `install`, `vad_reset`, `vad`, `transcribe` e `synthesize`.
- **Motores:** pywhispercpp, onnxruntime (Silero) e piper-tts.

Foi **substituído** pela voz nativa em Rust (`local_speech.rs`). Não entra no bundle e só está compilado quando a feature `local-speech` está desligada.

### Arquitetura

```
workers/                      (289 MB no disco; versionados só os 5 arquivos abaixo)
├─ speech_worker.py   (128)  loop de comandos JSON por stdin/stdout
├─ models.py          (134)  download, verificação SHA-256 e instalação atômica
├─ test_models.py     (82)   unittest (roda no CI)
├─ requirements.in    (4)    numpy, onnxruntime, piper-tts, pywhispercpp
├─ requirements.lock  (394)  gerado com uv, com hashes
├─ python/            [ignorado] CPython 3.12 portátil (scripts/setup-audio.sh)
├─ python-packages/   [ignorado] site-packages vendorizado (inclui piper-tts, GPL-3)
└─ __pycache__/       [lixo]
```

### O que falta (Etapa 4)
1. Apagar `workers/` e `scripts/setup-audio.sh`.
2. Remover as referências no Rust (ver `support-runtime` → Etapa 4).
3. **Scripts e CI:**
   - `scripts/build-linux.sh:6` exige `workers/python`;
   - `build-linux.sh:12` e `.github/workflows/check.yml:26` rodam `unittest` em `workers`.
4. Revisar as dependências do `.deb`: `libespeak-ng1` só faz sentido se `piper-phonemize` precisar dela em tempo de execução.
5. Atualizar `README.md`, `docs/{ARQUITETURA,EXECUCAO,VALIDACAO,ETAPA_ATUAL}.md` e as entradas de `.gitignore`.
6. Registrar a nota de GPL-3: `piper-phonemize`/espeak-ng continuam presentes via `local-speech`.

---

## 4. `tools/audio-bench/`

### Função
É o benchmark offline, fora do app, que embasou a escolha dos modelos de voz:
- **ASR:** sherpa-onnx Whisper turbo e small, contra candle Whisper large-v3-turbo.
- **TTS:** Piper cadu, faber e dii; Kokoro `pf_dora`; Supertonic 3.
- **VAD:** Silero v5.

Também serve de fonte local de modelos (`AURA_MODEL_SOURCE_ROOT` ou `tools/audio-bench/data/models`) e de fixtures para os 5 testes ignorados do runtime.

### Arquitetura

```
tools/audio-bench/            crate independente ([workspace] próprio)
├─ Cargo.toml    sherpa-onnx, piper-phonemize (GPL-3, só no bench), candle (opcional), clap, hound
├─ README.md     uso e licenças
├─ fetch.py      (449)  baixa modelos (~2 GB) e o corpus Common Voice pt (só stdlib + ffmpeg)
├─ report.py     (194)  data/results/*.json → data/results/summary.md
├─ src/lib.rs    (169)  utilidades comuns (corpus, WER, RSS, tempo)
├─ src/bin/
│  ├─ asr_sherpa.rs  (115)
│  ├─ asr_candle.rs  (238)
│  ├─ tts_sherpa.rs  (368)
│  └─ vad_sherpa.rs  (127)
├─ data/   [ignorado, 2,0 GB] manifest.json, models/, corpus/ (12 clipes), out/ (wavs), results/
└─ target/ [ignorado, 1,4 GB]
```

**Como rodar:**
1. `python3 tools/audio-bench/fetch.py`
2. `cargo build --release` (dentro da pasta)
3. Executar os binários (`asr-sherpa`, `tts-sherpa`, `vad-sherpa`, `asr-candle`).
4. `python3 report.py`

**Resultados** (`summary.md`, 2026-10-31):

| Candidato | WER / medida | Latência (p50) | RTF | RSS |
|---|---|---|---|---|
| Whisper turbo (sherpa, 8 threads) | WER 4,0% | 1,58 s | 0,31 | 1,77 GB |
| Whisper small | WER 23% | — | — | 0,92 GB |
| Candle turbo | WER 4,0% | — | — | 5,2 GB |
| Kokoro `pf_dora` | — | — | ~0,23 | 636 MB |
| Silero VAD | ~110 µs por frame | — | — | — |

- O candle turbo empata em WER, mas é cerca de 8× mais lento (RTF 2,66).
- O Kokoro foi o TTS adotado (Apache-2.0).

Escolhas: Whisper turbo (padrão), small (alternativa leve), Silero e Kokoro.

### O que falta
- Não está no CI. `data/` e `manifest.json` ficam fora do git, então os resultados não são reproduzíveis só pelo repositório.
- O corpus é pequeno (12 clipes). O RTF de clipes curtos é pessimista por causa da janela fixa de 30 s do Whisper.
- `summary.md` não tem seção de conclusão; a decisão está em `docs/BENCH_AUDIO.md`, que duplica parte do README.
- O candle só foi medido em f32.
- Independe de `workers/`; na Etapa 4 só entra na nota de licença GPL-3.

---

## 5. `src-tauri/`

### Função
É a casca desktop (crate `aura-desktop`). Abre a janela, hospeda o `support-runtime` **no mesmo processo** e faz a ponte dos comandos e eventos com a WebView. Também cuida do empacotamento (`.deb`, AppImage).

### Arquitetura

```
src-tauri/
├─ Cargo.toml             tauri 2 (sem plugins), support-core, support-runtime[local-speech]
├─ build.rs               tauri_build::build()
├─ tauri.conf.json        trunk serve/build, devUrl :1420, frontendDist ../dist, CSP restrita,
│                         withGlobalTauri, bundle deb+appimage, depends: libasound2,
│                         libespeak-ng1, libsecret-tools, tzdata (sem `resources`)
├─ capabilities/default.json   janela "main" → ["core:default"]
├─ gen/schemas/           gerado pelo tauri-build (não editar)
├─ icons/icon.png
└─ src/main.rs   (100)    setup: Runtime::open(app_data_dir, resources) → app.manage(Arc<Runtime>)
                          app_command(cmd,args)        → commands::dispatch(.., None)
                          app_stream(cmd,args,Channel) → dispatch com EventSink → Channel.send
                          comandos tipados legados (session_*, memory_search, model_validate)
                          RunEvent::Exit → runtime.cancel()
```

**Ponte:** `public/bridge.js` usa `window.__TAURI__.core.invoke('app_command' | 'app_stream')`. Os eventos chegam por um `ipc::Channel` por chamada, não por `emit`. Para `conversation_start`, o JS guarda os eventos em buffer até receber o ticket `{session_id, generation}` e filtra pela geração.

**Segurança:**
- a allowlist fica em `commands::dispatch`;
- as capabilities são só `core:default`, sem fs, shell ou opener;
- a CSP não permite rede externa a partir da WebView;
- `source_open` aceita só http/https sem credenciais.

### O que falta
- **Fase B:** `tauri-plugin-notification`: dependência, registro no builder, permissão em `capabilities/default.json`, e o consumo do evento de lembrete do runtime.
- **Fase C:** `tauri-plugin-dialog`, para escolher as pastas autorizadas, com a permissão correspondente. Abrir arquivo ou app deve passar pelo gate do runtime, não pela WebView.
- Os comandos tipados duplicados (`session_*`, `memory_search`, `model_validate`) não são usados pela UI; o `limit` de `memory_search` é ignorado. Candidatos a remoção.
- Os erros do stream só vão para `eprintln!`; não há log estruturado.
- A identidade ainda é do produto antigo (`identifier` `dev.aura.support`). Há só um ícone PNG e só alvos Linux.
- **Etapa 4:** revisar os `depends` do `.deb`, depois o build e a instalação do `.deb` de ponta a ponta.

---

## 6. `src/` (UI Leptos)

### Função
É a interface do usuário em Rust, compilada para wasm (CSR) com Trunk:
- conversa por voz e texto, com transcrição, atividade de ferramentas, fontes e cartão de confirmação de ação;
- histórico e memória;
- integrações (ativar e desativar; conexão Google);
- configurações (provedor LLM, áudio local, modelos, chaves).

### Arquitetura

```
src/
├─ main.rs   (4)     mount_to_body(App)
└─ app.rs    (1768)  tudo em um arquivo:
   ├─ bridge wasm_bindgen ("/public/bridge.js"): request, stream, dispose, orb_level
   ├─ Ui (Copy, ~40 RwSignal): page, settings, sessions, messages, state, running,
   │    generation/epoch/active_stream, integrations, tools, operation, models, speech…
   ├─ impl Ui: refresh_* (sessões, integrações, modelos, dispositivos), start (stream),
   │    stream_event (proteção contra eventos obsoletos), event (reducer), action (confirm/cancel/…), voz e avisos
   ├─ funções puras testáveis: accept_generation, Readiness, VoiceBlocker, NoticeGate, state_copy…
   ├─ App: inicialização + polling de 2 s + layout (nav com 4 páginas, alternadas com `hidden`)
   │   ├─ Conversation ── ActionCard, Sources, lista .tool-activity
   │   ├─ MemoryPage
   │   ├─ IntegrationsPage (lista genérica + toggles; painéis fixos do Google Calendar)
   │   └─ SettingsPage ── ArtifactCard, DeviceSelect
   └─ mod tests (25 testes de lógica pura)
```

**Integração:**
- `request()` chama `app_command` (JSON como string).
- A conversa usa `stream("conversation_start")`. Cada evento passa por `stream_event` e depois `Ui::event`.
- Os dados são `serde_json::Value` dinâmicos, sem tipos compartilhados com o runtime.

### O que falta
- **Monólito:** dividir em `bridge.rs`, `state.rs`, `events.rs`, `speech.rs`, `copy.rs` e `pages/{conversation,memory,integrations,settings}.rs`. Hoje o markup `view!` está em linhas muito longas.
- **Resquícios de Calendar:**
  - sinais `calendars` e `google_secret`;
  - painéis fixos "Google Calendar", "Sua agenda" e "Documentação do Google Calendar";
  - polling de `google_calendar.connecting`;
  - textos de exemplo sobre a agenda;
  - texto de conflitos no `ActionCard`;
  - `formatDate` em `bridge.js`;
  - `public/client.js`, que é só da prévia HTML.
- **Fase B:** telas de tarefas, lembretes e notas.
- **Fase C:** confirmação de ações de arquivo, URL e app, e escolha de pastas autorizadas.
- **Fase D (UI centrada no agente):**
  - uma timeline com itens de ferramenta, ação e fonte junto das mensagens (`ChatLine` hoje é só `{id, role, text}`; `tools` zera a cada run e não persiste);
  - a navegação reorganizada;
  - o `Ui` dividido em sub-stores;
  - `state_copy` revisado.
- **Testes:** não há testes de componente nem do reducer `Ui::event`; o CI só faz `trunk build`.
- **Acessibilidade:**
  - novas mensagens no transcript não são anunciadas;
  - não há gestão de foco ao trocar de página;
  - os ícones ✓/✗ e os glifos da navegação dependem só do símbolo visual.
