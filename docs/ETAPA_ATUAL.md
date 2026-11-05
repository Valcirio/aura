# Etapa atual e próxima implementação

Atualizado em **01/11/2026**. Este documento orienta a continuidade do desenvolvimento com base no [plano](PLANO.md), na [arquitetura](ARQUITETURA.md) e nas evidências do [registro de validação](VALIDACAO.md). O plano define os critérios de aceitação; a arquitetura define os limites entre os componentes.

## Etapa atual

> **Nota de status (31/10/2026).** A Etapa 3 está concluída: fala e áudio nativos (`LocalSpeech`, sherpa-onnx) estão integrados ao runtime e à UI e habilitados por padrão em `support-runtime` e `src-tauri`.
>
> - Whisper `turbo` int8 é o padrão; `small` int8 é opcional, com aviso de precisão na UI. Kokoro fp32 `pf_dora` usa o remap IPA pt-BR; a instalação de modelos na UI é guiada por catálogo.
> - A síntese e a reprodução usam um pipeline limitado por sentença, com pré-carga. O worker Python é excluído do bundle Tauri.
> - Arquivos do worker, setup e limpeza de empacotamento pertencem à Etapa 4, **pendente de aprovação**; nada disso foi removido.
> - **Verificações executadas em 01/11/2026** (host Linux do desenvolvedor, nesta ordem de execução):
>   - `cargo test --locked` (raiz): 25 aprovados. `support-runtime` com features padrão: 93 aprovados, 5 ignorados. `support-runtime --no-default-features`: 53 aprovados. `support-core` não foi reexecutado.
>   - `cargo test --locked --manifest-path crates/support-runtime/Cargo.toml -- --ignored --nocapture`: 5 aprovados (VAD com paridade Python em 40 quadros, TTS, ASR small, ASR turbo e o bootstrap abaixo). Nenhum `SKIP`: os arquivos do benchmark estavam presentes.
>   - **Bootstrap offline real** (`runtime::native_bootstrap_tests::native_bootstrap_reaches_ready_offline_from_verified_local_files`, ignorado): `Runtime::speech_prepare` com catálogo real (`whisper-turbo`, `silero-vad`, `kokoro-multi-lang-v1_0`), origem local por hardlinks do benchmark e pré-carga real; o transporte de rede é um `Source` de teste que recusa e conta qualquer tentativa. Resultado: fases `starting → copying → loading → ready`, **0 tentativas de rede**, três instalações, segunda chamada idempotente (≈45–50 s por execução). Sem os arquivos do benchmark o teste imprime `SKIP` e retorna sem exercitar o bootstrap (um processo "aprovado" então não prova cobertura com modelos).
>   - `node scripts/bridge.test.mjs`: 17 aprovados. `cargo check --locked --target wasm32-unknown-unknown` e `cargo check --manifest-path src-tauri/Cargo.toml`: concluídos. `cargo build --locked ... --bin aura-runtime`: concluído.
>   - `python3 scripts/verify-runtime.py`: falhava na asserção do id 11 (`model_install` com id inexistente esperava `MODEL_INVALID`; o contrato atual devolve `INVALID_INPUT`, "Modelo fora do catálogo"). Asserção atualizada; depois disso, aprovado (14 comandos). Ele não cobre `provider_check` nem `speech_prepare`.
>   - Formatação: após `rustfmt --edition 2024` em `src/app.rs`, `crates/support-runtime/src/audio.rs` e `crates/support-runtime/src/commands.rs`, `cargo fmt --check` (raiz) e `cargo fmt --manifest-path crates/support-runtime/Cargo.toml --check` passaram.
>   - **Probe do provedor:** `provider_check` via `aura-runtime` NDJSON, com diretório de dados temporário e configuração padrão (Ollama, `http://127.0.0.1:11434`, `qwen3:4b`), terminou com o processo em código 0 e a resposta `{"error":{"code":"SERVICE_UNREACHABLE","message":"Não foi possível conectar ao serviço","retryable":true},"id":3}`; `provider_status` retornou `null`. Não havia processo escutando em 127.0.0.1:11434. Nenhum provedor foi instalado ou iniciado; não se atribui causa a falhas de sessão do usuário. Os contratos estão em CONTRATOS.md.
>   - **Desktop:** `WEBKIT_DISABLE_DMABUF_RENDERER=1 timeout 90s bash scripts/dev.sh` (`DISPLAY=:0`): Trunk e `cargo tauri dev` compilaram e `target/debug/aura-desktop` chegou a "Running"; o processo foi encerrado pelo `timeout` (código 124) sem erro de inicialização no log. O log só mostra mensagens ALSA/OSS (`Cannot open device /dev/dsp`) e reconstruções repetidas do Trunk ("applying new distribution"). Nenhuma interação com a janela foi controlada ou observada: abertura da WebView, IPC e UI **não** foram verificados por esse log.
> - **Não verificados:** conversa por voz de ponta a ponta com áudio físico, parada em até 200 ms, interação com a WebView e a UI, preparação de fala pela UI, `provider_check` contra um provedor em execução, latência e geração/instalação do DEB.
> - O texto abaixo é planejamento operacional anterior; onde menciona o worker Python ou Piper como caminho atual, está superado pela Etapa 3.

O projeto está na **integração e validação operacional do MVP Linux**. Os principais componentes estão implementados e possuem testes locais, mas a experiência completa na janela Tauri ainda precisa ser demonstrada em uma máquina com rede, modelos, dispositivos de áudio e credenciais configurados.

| Escopo do plano | Implementação disponível | O que falta comprovar |
| --- | --- | --- |
| Fundação e frontend | Tauri 2, Leptos, dispatcher IPC e quatro páginas: Conversa, Memória, Integrações e Configurações | Build WASM, abertura da WebView, canais IPC, estados visuais e acessibilidade |
| Áudio local | Captura/reprodução CPAL, resampler, Silero VAD, `LocalSpeech` nativo (sherpa-onnx) com Whisper `turbo`/`small` e Kokoro `pf_dora`, instalação por catálogo na UI | Conversa com microfone e reprodução física, parada ≤200 ms, latência |
| Modelos locais e APIs | Adaptadores Ollama/API compatível, streaming, probe de capacidades e cancelamento | Probe e conversa com um modelo local e uma API configurados |
| Agentes, serviços e memória | Coordenador de ferramentas, registro de serviços, SQLite/FTS, documentação oficial e Brave Search | Recuperação durante conversa real, atualização pela rede e pesquisa com credencial |
| Google Calendar | OAuth Desktop/PKCE, consultas, proposta persistida, confirmação e reconciliação | Login real, consulta de agendas e criação controlada em agenda de testes |
| Distribuição | Scripts de setup/dev/build e configuração DEB/AppImage; worker excluído do bundle | Geração, instalação limpa e atualização do pacote Linux (DEB não testado); limpeza na Etapa 4 pendente |

(Histórico) A validação registrada em VALIDACAO.md aprovou **51 testes automatizados**, o smoke de nove comandos do backend, a compilação do runtime com CPAL e a verificação de tipos do frontend no host. Isso não comprova execução WASM, inferência neural ou operações reais no Google Calendar.

Os bloqueios encontrados foram dependências ausentes no cache, DNS indisponível para downloads e restrições do sandbox ao Chromium. Não há medições de latência, qualidade de voz ou consumo dos motores. O detalhamento permanece em [VALIDACAO.md](VALIDACAO.md).

## Refatoração para agente generalista — Fase A

**Concluído:**

- especificações de ferramentas com efeitos (`Effect`, `ToolSpec`) e validação de esquema dos argumentos;
- registro de integrações (`memory`, `knowledge`, `web`, `google_calendar`) com ativação/desativação por `integrations_disabled`;
- Operations v2 genéricas (`integration`, `tool`, `effect`, `summary`, `fields`);
- um único loop de agente com limites e eventos `tool.started`/`tool.completed`;
- commands `integration_*` (`service_*` como aliases obsoletos);
- UI com texto genérico, página Integrações e atividade de ferramentas.

**Próximo:** Fase B (tarefas, lembretes, notas), Fase C (arquivos/sistema), Fase D (UI) e, em seguida, Etapa 4 (remoção do Python).

## Próxima implementação prioritária

**Consolidar a execução desktop e acrescentar um smoke de integração da aplicação real**, corrigindo os problemas encontrados até demonstrar uma conversa completa e uma criação confirmada no Calendar. Essa entrega fecha os critérios operacionais já previstos no plano.

O primeiro resultado deve ser a janela Tauri executando o frontend WASM e realizando uma chamada IPC real. Depois, a mesma instalação deve demonstrar o fluxo:

```text
microfone → VAD → Whisper → LLM/ferramentas → Kokoro (por sentença, com pré-carga) → reprodução → nova escuta
```

### Ordem de execução e entregáveis

| Ordem | Trabalho seguinte | Componentes envolvidos | Critério para avançar |
| --- | --- | --- | --- |
| 1 | Preparar o Linux conforme EXECUCAO.md, compilar WASM/Tauri e corrigir falhas de build, recursos ou IPC | `src-tauri/`, `src/app.rs`, `public/bridge.js`, `Trunk.toml`, `scripts/dev.sh` | Janela abre, páginas navegam e uma sessão criada pela UI continua disponível após reiniciar |
| 2 | Instalar os modelos nativos pelo catálogo na UI; verificar status, seleção de dispositivos e amostras da voz | `local_speech.rs`, `models.rs`, `speech.rs`, `audio.rs` | Whisper e VAD carregam; Kokoro reproduz áudio; falha de instalação preserva a versão anterior |
| 3 | Validar provedor e conversa por texto, depois por voz; corrigir integração e cancelamento | `provider.rs`, `runtime.rs`, `audio.rs`, `src/app.rs` | Probe passa em Ollama e API compatível; fala recebe resposta audível, silêncio não cria mensagem e Parar libera os dispositivos |
| 4 | Validar memória, documentação oficial e pesquisa complementar | `knowledge.rs`, `integrations.rs`, `support-core/src/store.rs` | Conversa anterior é recuperada após reinício; fontes são identificadas; falha de atualização mantém conteúdo anterior; Brave funciona quando configurado |
| 5 | Conectar conta de teste e validar consultas e criação confirmada | `google.rs`, `integrations.rs`, operações persistidas e tela Serviços | Evento só é criado após confirmação; conta, sessão, fuso e disponibilidade são respeitados; resultado incerto é reconciliado sem duplicação |
| 6 | Adicionar smoke do desktop real e registrar resultados dos ensaios | `scripts/`, `.github/workflows/check.yml`, `docs/VALIDACAO.md` | O smoke verifica WebView/IPC e persistência; os ensaios com áudio, OAuth e criação registram evidência própria |

Os caminhos Rust sem prefixo na tabela pertencem a `crates/support-runtime/src/`. O smoke desktop será um artefato novo; o atual `scripts/verify-runtime.py` cobre o backend e `scripts/verify-preview.mjs` cobre a prévia estática. Ambos continuam úteis, mas não substituem esse novo ensaio.

O smoke automatizado deve funcionar sem credenciais de produção nem gravações externas. Login e criação controlada devem ser exercitados separadamente com conta e agenda de testes, incluindo confirmação explícita. Segredos devem ser configurados pela interface e permanecer fora dos relatórios.

### Critérios de conclusão desta etapa

- Build WASM e Tauri passam; a janela executa o frontend real e os canais IPC.
- Uma conversa em português percorre os motores locais, reproduz resposta com a voz padrão e volta à escuta. O círculo acompanha a saída, e a UI distingue escuta, processamento e resposta.
- Cancelamento interrompe captura/reprodução e impede eventos atrasados de reativar a saída. Interromper áudio durante uma operação externa não deve apresentar essa operação como desfeita.
- Calendar permite consulta e criação confirmada em agenda de testes. Revisão, expiração, conta divergente, mudança de disponibilidade e reconciliação seguem os cenários de PLANO.md.
- Memória permanece pesquisável após reinício; falhas de rede/modelos geram estados compreensíveis sem destruir dados existentes.
- A UI é verificada em 360, 390, 768 e 1280 px, com teclado, zoom de 200% e movimento reduzido.
- As suítes existentes continuam aprovadas e o registro de validação informa comandos, versões, hardware, resultados e pendências, sem marcar ensaios não executados como aprovados.

## Limites arquiteturais a preservar

O áudio e a inferência de fala rodam no runtime Rust (CPAL e `LocalSpeech`/sherpa-onnx, features habilitadas por padrão). O worker Python permanece no repositório apenas até a limpeza da Etapa 4, pendente de aprovação. PCM não deve atravessar a WebView. Leptos controla a interface e recebe eventos pelo dispatcher Tauri.

Novos serviços devem usar `ServiceAdapter` e `ServiceExecutor`, mantendo o coordenador e o pipeline de áudio independentes dos produtos. Nesta etapa, o serviço funcional permanece o Google Calendar, com uma conta conectada por vez.

O modelo pode consultar e preparar propostas, mas não recebe uma ferramenta de confirmação. A gravação continua condicionada ao controle de UI/voz, com operação persistida, vínculo à conta/sessão e identificador estável. Um timeout deve preservar a incerteza até reconciliação; repetir a geração de texto não autoriza repetir uma gravação.

Conversas e documentação conservam índices separados. Chaves e tokens permanecem no cofre do sistema ou em RAM, sem exposição à WebView ou aos registros de validação.

## Verificação de não regressão

Para alterações de integração, execute a baseline abaixo e acrescente os checks do escopo alterado previstos em PLANO.md:

```bash
cargo test --locked --manifest-path crates/support-core/Cargo.toml
cargo test --locked --manifest-path crates/support-runtime/Cargo.toml
python3 -m unittest discover -s workers -p 'test_*.py'  # legado, até a Etapa 4
node scripts/bridge.test.mjs
cargo build --locked --manifest-path crates/support-runtime/Cargo.toml --bin aura-runtime
python3 scripts/verify-runtime.py
```

No ambiente preparado, execute também `cargo check --locked --target wasm32-unknown-unknown`, `cargo check --manifest-path src-tauri/Cargo.toml` e o aplicativo via `bash scripts/dev.sh`. Preserve os dez testes originais e acrescente testes direcionados para defeitos corrigidos. A referência de 51 testes descreve a baseline registrada, não limita a evolução da suíte.

## Etapa seguinte após a integração

Após aprovar os critérios acima, avance para **distribuição e avaliação de desempenho em Linux**: gerar DEB/AppImage com `scripts/build-linux.sh`, testar instalação limpa/atualização e registrar as medições definidas no plano. As metas são p95 de até cinco segundos até o primeiro áudio sem ferramentas, 200 ms para parada explícita, 150 ms para busca em dez mil mensagens e cem turnos reais sem crescimento contínuo de recursos. Todas ainda precisam ser medidas.

Expansões de produto devem ser priorizadas depois desses critérios. Windows/macOS, múltiplas contas simultâneas, interrupção natural com cancelamento de eco, edição/exclusão de eventos, convites, novas recorrências, Meet e embeddings continuam fora do MVP, conforme PLANO.md.

Ao concluir cada entrega, atualize este documento e VALIDACAO.md. Mudanças de escopo ou de componentes também devem ser refletidas em PLANO.md e ARQUITETURA.md, preservando a distinção entre código implementado, verificação automatizada e validação operacional.
