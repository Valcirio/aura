# Aura

Agente desktop de suporte por voz com **Tauri 2 + Leptos**, preparado para múltiplos serviços. A primeira integração é o **Google Calendar**: consulta de agendas, eventos e disponibilidade, e criação de compromissos **somente após confirmação**.

O pipeline implementado é **microfone → Silero VAD → Whisper local → LLM + ferramentas → Piper local → reprodução nativa**. O frontend oferece Conversa, Memória, Serviços e Configurações, com círculo ligado ao nível de áudio e vozes brasileiras Cadu/Faber.

## Executar o aplicativo

Requer Linux, Rust estável, dependências nativas Tauri/ALSA e acesso à rede para a instalação inicial. O roteiro completo, incluindo Google Cloud e OAuth, está em **[docs/EXECUCAO.md](docs/EXECUCAO.md)**.

```bash
rustup target add wasm32-unknown-unknown
cargo install trunk --locked --root .tools
cargo install tauri-cli --version '^2' --locked --root .tools
bash scripts/setup-audio.sh
bash scripts/dev.sh
```

O setup de áudio requer [uv](https://docs.astral.sh/uv/getting-started/installation/). Ele instala Python 3.12 e os motores dentro do projeto; o empacotamento inclui esses recursos. O aplicativo oferece o download explícito e verificado dos quatro artefatos de áudio.

Na primeira abertura:

1. Em **Configurações**, escolha Ollama ou API compatível, salve e teste a compatibilidade real do modelo.
2. Instale Whisper base, Silero VAD e pelo menos uma voz Piper.
3. Configure o cliente OAuth Desktop do seu projeto Google Cloud e conecte a conta em **Serviços**.
4. Carregue as agendas, escolha a padrão e atualize a documentação oficial.
5. Inicie uma conversa ou escreva uma mensagem. Confira a proposta antes de salvar um compromisso.

Para Ollama, prepare um modelo com ferramentas, por exemplo `ollama pull qwen3:4b`. APIs remotas exigem HTTPS; chaves e tokens ficam no cofre do sistema ou somente na RAM quando ele não está disponível. Brave Search é opcional e configurável pela interface.

## O que foi implementado

- Backend Rust independente da WebView: provedor Ollama/API, parser de streaming, probe de ferramentas/português/cancelamento e orçamento de contexto.
- Registro de serviços com `ServiceAdapter` e `ServiceExecutor`; um teste registra outro produto sem alterar o coordenador ou o áudio.
- Google Calendar REST v3, OAuth Desktop com PKCE, renovação de token, uma conta por vez e seleção de agenda.
- Propostas persistidas, confirmação vinculada a conta/sessão, detecção de mudanças de disponibilidade e reconciliação por identificador estável após timeout.
- Worker Python persistente para whisper.cpp, Silero ONNX e Piper CPU; instalação de modelos com revisão e verificação de integridade.
- Áudio nativo CPAL, normalização, segmentação automática, filas limitadas, cancelamento e medição de saída associada ao clock do dispositivo.
- SQLite com migração do banco anterior, sessões paginadas, busca sem acentos e por data, documentos separados das conversas e registro de operações/turnos.
- Documentação oficial indexada e pesquisa pública complementar pela API Brave.
- UI Leptos reativa, confirmação por voz/botão e layout responsivo.

## Validação e limites atuais

O domínio, o runtime com CPAL e o frontend no alvo host foram compilados. Foram executados testes automatizados e um smoke que chama o backend real por NDJSON, inicia o worker Python e reabre o banco. Detalhes e comandos em **[VALIDACAO.md](docs/VALIDACAO.md)**.

**A janela Tauri e o build WASM ainda não foram validados neste ambiente**: faltam dependências em cache e a rede de shell está bloqueada. A instalação dos motores Python também foi tentada e bloqueada por DNS. Não foram realizados OAuth com uma conta real, gravação de eventos reais, inferência neural ou benchmark de latência. O código inclui os clientes reais e a configuração pela interface; esses ensaios precisam de rede, modelos e credenciais configurados.

## Testes

```bash
cargo test --locked --manifest-path crates/support-core/Cargo.toml
cargo test --locked --manifest-path crates/support-runtime/Cargo.toml
cargo check --locked
python3 -m unittest discover -s workers -p 'test_*.py'
node scripts/bridge.test.mjs
cargo build --locked --manifest-path crates/support-runtime/Cargo.toml --bin aura-runtime
python3 scripts/verify-runtime.py
```

Os manifests são independentes para testar o backend sem dependências da WebView. O harness `aura-runtime` recebe linhas `{id, command, args}` e não abre portas HTTP públicas. Seu banco de desenvolvimento usa `.local-data/`, separado do `app_data_dir()/memory.db` do Tauri.

## Design e distribuição

A prévia HTML continua disponível com `python3 -m http.server 8080 --bind 127.0.0.1`, em **http://127.0.0.1:8080/design/**. Ela testa apenas o design, microfone e reprodução de arquivo; a integração real roda na janela Tauri.

Para preparar pacotes Linux: `bash scripts/build-linux.sh`. O script exige os motores Python instalados, executa testes e chama Tauri para gerar DEB/AppImage. A geração e instalação desses pacotes ainda precisam ser verificadas em uma máquina com as dependências disponíveis; compatibilidade entre distribuições depende do ambiente de build.

- [Arquitetura](docs/ARQUITETURA.md)
- [Execução e configuração Google](docs/EXECUCAO.md)
- [Contratos do backend](docs/CONTRATOS.md)
- [Modelos e fontes](docs/MODELOS.md)
- [Plano e critérios de aceitação](docs/PLANO.md)
- [Etapa atual e próxima implementação](docs/ETAPA_ATUAL.md)
- [Design](docs/DESIGN.md)
- [Validação](docs/VALIDACAO.md)
