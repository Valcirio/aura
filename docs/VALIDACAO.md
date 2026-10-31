# Registro de validação

Executado em 29/10/2026 no Linux. Os dez testes anteriores do domínio foram preservados. As verificações abaixo cobrem código real do backend, persistência, protocolos e componentes; os limites de execução estão registrados separadamente.

| Verificação | Resultado | Alcance |
| --- | --- | --- |
| `cargo test --locked --manifest-path crates/support-core/Cargo.toml` | **26 testes aprovados** | Domínio, SQLite, migração, busca, operações, datas e segmentação |
| `cargo test --locked --manifest-path crates/support-runtime/Cargo.toml` | **13 testes aprovados** | Streaming, ferramentas, confirmação, cancelamento e integração entre módulos |
| `python3 -m unittest discover -s workers -p 'test_*.py'` | **8 testes aprovados** | Protocolo persistente, catálogo e instalação atômica com integridade |
| `node scripts/bridge.test.mjs` | **4 testes aprovados** | Ponte IPC, erros, streaming e apresentação de datas |
| Build de `aura-runtime` e `python3 scripts/verify-runtime.py` | **Aprovados** | Nove comandos pelo dispatcher real, worker Python e reabertura do SQLite |
| `cargo check --locked` | **Aprovado** | Tipos e macros Leptos no alvo host; não executa WASM |
| Formatação Rust nos quatro manifests | **Aprovada** | Frontend, domínio, runtime e casca Tauri |
| Sintaxe Python/JavaScript/shell e parsing JSON | **Aprovados** | Scripts, workers, ponte e configuração |
| Compilação Tauri | **Bloqueada** | Crate `tauri` ausente no cache e DNS indisponível no shell |
| Build WASM/Trunk | **Bloqueado** | Alvo WASM e Trunk ausentes; tentativa de build-std também exige `wasip1`, ausente no cache |
| Instalação dos motores Python | **Bloqueada** | Download de Python gerenciado pelo `uv` não resolve DNS |
| Smoke visual no Chromium | **Bloqueado** | Browser encerra com `setsockopt: Operation not permitted` no sandbox |

São **51 testes automatizados**, além do smoke do runtime. Neste ambiente, os comandos Cargo usaram `--offline` e `CARGO_HOME=/tmp/aura-cargo`, com cache existente e diretório gravável temporário. Em uma máquina com rede, use os comandos normais do README.

## O que os testes demonstram

- Conversas continuam pesquisáveis após reabrir o banco; filtros de sessão/data, normalização de acentos, exclusão e migração preservam os invariantes existentes.
- Datas de dia inteiro têm fim exclusivo; horários ambíguos ou inexistentes em transições de fuso são recusados.
- Uma confirmação precisa corresponder à conta, sessão, proposta e validade. Mudança de disponibilidade exige revisão; cliques repetidos não repetem a criação.
- Timeout preserva um resultado desconhecido. Reconciliação consulta o identificador estável; uma nova tentativa requer confirmação e reutiliza esse identificador.
- Parsers aceitam UTF-8 dividido e argumentos de ferramentas fragmentados; streams truncados são recusados. O orçamento de contexto preserva os pares de ferramenta.
- Um serviço adicional pode registrar manifesto e executor sem alterar o coordenador ou o áudio.
- Eventos atrasados de cem emissores cancelados não reativam a saída. Esse teste não representa cem conversas com inferência neural.
- A instalação detecta tamanho/hash inválidos e caminhos impróprios; uma falha preserva a versão anterior. Dois comandos usam o mesmo processo worker.

As chamadas do Calendar nos testes usam implementações controladas da interface `CalendarApi`. Elas verificam o comportamento do cliente e das operações sem gravar eventos reais. O smoke inicia o worker em modo de status, sem carregar redes neurais.

## Validações operacionais restantes

Não foram executados a janela Tauri, inferência Whisper/Silero/Piper, conversa com LLM real, OAuth com conta real, criação real de evento, Brave Search com credencial, microfone/reprodução física ou geração/instalação de DEB/AppImage. O runtime com CPAL foi compilado; isso não comprova acesso a dispositivos ou desempenho acústico.

Não há medidas de latência, RAM dos motores, inteligibilidade ou precisão de transcrição. As metas e os cenários de aceitação estão em [PLANO.md](PLANO.md); a preparação da máquina e do Google Cloud está em [EXECUCAO.md](EXECUCAO.md). Esses ensaios exigem rede, modelos, dispositivos e credenciais configurados pela interface.

O `cargo check` do frontend no host não valida carregamento WASM, CSP, canais IPC ou renderização em WebView. O script visual disponível valida a prévia estática, não substitui o ensaio do aplicativo desktop.
