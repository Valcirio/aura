# Etapa atual e próxima implementação

Atualizado em **31/10/2026**. Este documento orienta a continuidade do desenvolvimento com base no [plano](PLANO.md), na [arquitetura](ARQUITETURA.md) e nas evidências do [registro de validação](VALIDACAO.md). O plano define os critérios de aceitação; a arquitetura define os limites entre os componentes.

## Etapa atual

> **Nota de status (31/10/2026).** A Etapa 2 nativa está concluída: `LocalSpeech` em `support-runtime`, disponível apenas com a feature `local-speech`, ainda não integrado ao runtime de áudio nem à UI. O caminho da aplicação continua usando o worker Python.
>
> - Whisper `turbo` int8 com 8 threads é o padrão; `small` int8 com 4 threads é selecionável via `AsrModel`. VAD Silero e Kokoro fp32 `pf_dora`, sid 42, com remap IPA pt-BR estão implementados.
> - Validação: `cargo test --manifest-path crates/support-runtime/Cargo.toml --features local-speech -- --include-ignored --nocapture --test-threads=1` resultou em **45 aprovados, 0 ignorados**; testes padrão: **13 aprovados**; formatação limpa.
> - A Etapa 3, integração ao aplicativo, está pendente de aprovação do usuário.
> - O texto abaixo é planejamento e validação operacional anteriores, não uma descrição atualizada da implementação nativa.

O projeto está na **integração e validação operacional do MVP Linux**. Os principais componentes estão implementados e possuem testes locais, mas a experiência completa na janela Tauri ainda precisa ser demonstrada em uma máquina com rede, modelos, dispositivos de áudio e credenciais configurados.

| Escopo do plano | Implementação disponível | O que falta comprovar |
| --- | --- | --- |
| Fundação e frontend | Tauri 2, Leptos, dispatcher IPC e quatro páginas: Conversa, Memória, Serviços e Configurações | Build WASM, abertura da WebView, canais IPC, estados visuais e acessibilidade |
| Áudio local | Captura/reprodução CPAL, resampler, Silero VAD, worker persistente, Whisper base e Piper Cadu/Faber | Instalação dos motores/pesos, inferência, conversa com microfone e reprodução física |
| Modelos locais e APIs | Adaptadores Ollama/API compatível, streaming, probe de capacidades e cancelamento | Probe e conversa com um modelo local e uma API configurados |
| Agentes, serviços e memória | Coordenador de ferramentas, registro de serviços, SQLite/FTS, documentação oficial e Brave Search | Recuperação durante conversa real, atualização pela rede e pesquisa com credencial |
| Google Calendar | OAuth Desktop/PKCE, consultas, proposta persistida, confirmação e reconciliação | Login real, consulta de agendas e criação controlada em agenda de testes |
| Distribuição | Scripts de setup/dev/build e configuração DEB/AppImage | Geração, instalação limpa e atualização do pacote Linux |

A última validação registrada aprovou **51 testes automatizados**, o smoke de nove comandos do backend, a compilação do runtime com CPAL e a verificação de tipos do frontend no host. Isso não comprova execução WASM, inferência neural ou operações reais no Google Calendar.

Os bloqueios encontrados foram dependências ausentes no cache, DNS indisponível para downloads e restrições do sandbox ao Chromium. Não há medições de latência, qualidade de voz ou consumo dos motores. O detalhamento permanece em [VALIDACAO.md](VALIDACAO.md).

## Próxima implementação prioritária

**Consolidar a execução desktop e acrescentar um smoke de integração da aplicação real**, corrigindo os problemas encontrados até demonstrar uma conversa completa e uma criação confirmada no Calendar. Essa entrega fecha os critérios operacionais já previstos no plano.

O primeiro resultado deve ser a janela Tauri executando o frontend WASM e realizando uma chamada IPC real. Depois, a mesma instalação deve demonstrar o fluxo:

```text
microfone → VAD → Whisper → LLM/ferramentas → Piper → reprodução → nova escuta
```

### Ordem de execução e entregáveis

| Ordem | Trabalho seguinte | Componentes envolvidos | Critério para avançar |
| --- | --- | --- | --- |
| 1 | Preparar o Linux conforme EXECUCAO.md, compilar WASM/Tauri e corrigir falhas de build, recursos ou IPC | `src-tauri/`, `src/app.rs`, `public/bridge.js`, `Trunk.toml`, `scripts/dev.sh` | Janela abre, páginas navegam e uma sessão criada pela UI continua disponível após reiniciar |
| 2 | Instalar o runtime Python e os quatro artefatos; verificar status, seleção de dispositivos e amostras das duas vozes | `scripts/setup-audio.sh`, `workers/`, `speech.rs`, `audio.rs` | Whisper e VAD carregam; Cadu e Faber reproduzem áudio; falha de instalação preserva a versão anterior |
| 3 | Validar provedor e conversa por texto, depois por voz; corrigir integração e cancelamento | `provider.rs`, `runtime.rs`, `audio.rs`, `src/app.rs` | Probe passa em Ollama e API compatível; fala recebe resposta audível, silêncio não cria mensagem e Parar libera os dispositivos |
| 4 | Validar memória, documentação oficial e pesquisa complementar | `knowledge.rs`, `integrations.rs`, `support-core/src/store.rs` | Conversa anterior é recuperada após reinício; fontes são identificadas; falha de atualização mantém conteúdo anterior; Brave funciona quando configurado |
| 5 | Conectar conta de teste e validar consultas e criação confirmada | `google.rs`, `integrations.rs`, operações persistidas e tela Serviços | Evento só é criado após confirmação; conta, sessão, fuso e disponibilidade são respeitados; resultado incerto é reconciliado sem duplicação |
| 6 | Adicionar smoke do desktop real e registrar resultados dos ensaios | `scripts/`, `.github/workflows/check.yml`, `docs/VALIDACAO.md` | O smoke verifica WebView/IPC e persistência; os ensaios com áudio, OAuth e criação registram evidência própria |

Os caminhos Rust sem prefixo na tabela pertencem a `crates/support-runtime/src/`. O smoke desktop será um artefato novo; o atual `scripts/verify-runtime.py` cobre o backend e `scripts/verify-preview.mjs` cobre a prévia estática. Ambos continuam úteis, mas não substituem esse novo ensaio.

O smoke automatizado deve funcionar sem credenciais de produção nem gravações externas. Login e criação controlada devem ser exercitados separadamente com conta e agenda de testes, incluindo confirmação explícita. Segredos devem ser configurados pela interface e permanecer fora dos relatórios.

### Critérios de conclusão desta etapa

- Build WASM e Tauri passam; a janela executa o frontend real e os canais IPC.
- Uma conversa em português percorre os motores locais, reproduz resposta com cada voz e volta à escuta. O círculo acompanha a saída, e a UI distingue escuta, processamento e resposta.
- Cancelamento interrompe captura/reprodução e impede eventos atrasados de reativar a saída. Interromper áudio durante uma operação externa não deve apresentar essa operação como desfeita.
- Calendar permite consulta e criação confirmada em agenda de testes. Revisão, expiração, conta divergente, mudança de disponibilidade e reconciliação seguem os cenários de PLANO.md.
- Memória permanece pesquisável após reinício; falhas de rede/modelos geram estados compreensíveis sem destruir dados existentes.
- A UI é verificada em 360, 390, 768 e 1280 px, com teclado, zoom de 200% e movimento reduzido.
- As suítes existentes continuam aprovadas e o registro de validação informa comandos, versões, hardware, resultados e pendências, sem marcar ensaios não executados como aprovados.

## Limites arquiteturais a preservar

O áudio continua no runtime Rust/CPAL; o worker Python mantém a inferência e a instalação dos modelos. O `LocalSpeech` nativo (feature opt-in `local-speech`) ainda não está integrado; a aplicação usa o worker Python. PCM não deve atravessar a WebView. Leptos controla a interface e recebe eventos pelo dispatcher Tauri.

Novos serviços devem usar `ServiceAdapter` e `ServiceExecutor`, mantendo o coordenador e o pipeline de áudio independentes dos produtos. Nesta etapa, o serviço funcional permanece o Google Calendar, com uma conta conectada por vez.

O modelo pode consultar e preparar propostas, mas não recebe uma ferramenta de confirmação. A gravação continua condicionada ao controle de UI/voz, com operação persistida, vínculo à conta/sessão e identificador estável. Um timeout deve preservar a incerteza até reconciliação; repetir a geração de texto não autoriza repetir uma gravação.

Conversas e documentação conservam índices separados. Chaves e tokens permanecem no cofre do sistema ou em RAM, sem exposição à WebView ou aos registros de validação.

## Verificação de não regressão

Para alterações de integração, execute a baseline abaixo e acrescente os checks do escopo alterado previstos em PLANO.md:

```bash
cargo test --locked --manifest-path crates/support-core/Cargo.toml
cargo test --locked --manifest-path crates/support-runtime/Cargo.toml
python3 -m unittest discover -s workers -p 'test_*.py'
node scripts/bridge.test.mjs
cargo build --locked --manifest-path crates/support-runtime/Cargo.toml --bin aura-runtime
python3 scripts/verify-runtime.py
```

No ambiente preparado, execute também `cargo check --locked --target wasm32-unknown-unknown`, `cargo check --manifest-path src-tauri/Cargo.toml` e o aplicativo via `bash scripts/dev.sh`. Preserve os dez testes originais e acrescente testes direcionados para defeitos corrigidos. A referência de 51 testes descreve a baseline registrada, não limita a evolução da suíte.

## Etapa seguinte após a integração

Após aprovar os critérios acima, avance para **distribuição e avaliação de desempenho em Linux**: gerar DEB/AppImage com `scripts/build-linux.sh`, testar instalação limpa/atualização e registrar as medições definidas no plano. As metas são p95 de até cinco segundos até o primeiro áudio sem ferramentas, 200 ms para parada explícita, 150 ms para busca em dez mil mensagens e cem turnos reais sem crescimento contínuo de recursos. Todas ainda precisam ser medidas.

Expansões de produto devem ser priorizadas depois desses critérios. Windows/macOS, múltiplas contas simultâneas, interrupção natural com cancelamento de eco, edição/exclusão de eventos, convites, novas recorrências, Meet e embeddings continuam fora do MVP, conforme PLANO.md.

Ao concluir cada entrega, atualize este documento e VALIDACAO.md. Mudanças de escopo ou de componentes também devem ser refletidas em PLANO.md e ARQUITETURA.md, preservando a distinção entre código implementado, verificação automatizada e validação operacional.
