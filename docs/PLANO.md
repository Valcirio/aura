# Implementação e gates de desenvolvimento

O plano foi aplicado com Linux primeiro, LLM local/API, Piper padrão, turnos automáticos e Google Calendar como primeiro serviço. As etapas abaixo distinguem implementação de código e validação operacional; ter um cliente escrito não equivale a ter completado OAuth ou executado inferência real.

## Estado por escopo

| Etapa | Código entregue | Evidência disponível | Gate operacional pendente |
|---|---|---|---|
| Fundação | Runtime independente, dispatcher IPC, migração versionada, UI Leptos | Compilação host, testes de migração e persistência | Compilar WASM/Tauri e abrir a janela |
| Áudio local | CPAL, resampler, VAD, worker persistente, Whisper/Piper, catálogo e instalação atômica | Compilação nativa, testes de segmentação, protocolo, hashes e falhas de instalação | Instalar motores/pesos e testar microfone/alto-falante reais |
| Provedores | Ollama/API, SSE/NDJSON, probe real, cancelamento e contexto | Testes de fragmentação UTF-8, tools e orçamento | Executar probe contra um modelo local e uma API configurados |
| Serviços e conhecimento | Registro de adaptadores/executores, FTS separado, atualização oficial e Brave | Teste de outro produto registrado e busca local | Atualizar documentação pela rede e testar Brave com chave |
| Calendar | OAuth Desktop/PKCE, tokens, agendas, eventos e disponibilidade | Validação de callback e contratos; testes com API fake | Autorizar conta de teste e consultar dados reais |
| Criação confirmada | Proposta, expiração, claim atômico, conflito, reconciliação e nova tentativa com mesmo ID | Testes sem gravação antes de confirmar, conta divergente, timeout e ID estável | Criar um evento controlado em agenda de testes |
| Frontend | Quatro páginas, mensagens, configuração, fontes, vozes e confirmação | Compilação host, quatro testes da ponte JS | Testes de WebView, acessibilidade e visual em quatro larguras |
| Distribuição | Scripts de setup/dev/build, Python portátil e configuração DEB/AppImage | Sintaxe dos scripts e configuração verificadas | Gerar, instalar e atualizar o pacote em Linux de referência |

## Sequência de validação em máquina com rede

1. Preparar dependências nativas, alvo WASM, Trunk e CLI Tauri; abrir `scripts/dev.sh`.
2. Executar `scripts/setup-audio.sh`, instalar os quatro artefatos na interface e testar cada voz.
3. Salvar provedor, executar o probe e testar conversa sem Google conectado.
4. Configurar projeto Google Cloud/cliente Desktop, autorizar a conta e carregar agendas.
5. Atualizar documentação oficial e configurar Brave, se desejado.
6. Exercitar consulta, memória e criação confirmada nos cenários abaixo.
7. Gerar o pacote com `scripts/build-linux.sh` e repetir os cenários em instalação limpa.

A ordem preserva um ponto funcional a cada passo. Nenhuma mudança em áudio precisa esperar a disponibilidade da API Calendar para seus testes de domínio.

## Cenários de aceitação

- Fala em português → texto → resposta audível com Cadu e Faber; silêncio não gera mensagem.
- Ao terminar a resposta, a escuta volta; Parar interrompe o áudio e libera o microfone.
- Modelos sem ferramentas ou sem streaming ficam bloqueados com motivo claro.
- “Quais compromissos tenho amanhã?” consulta a conta e usa o fuso configurado.
- “Estou livre das 14h às 17h?” não confunde erro de agenda com ausência de compromissos.
- “Crie uma revisão amanhã das 14h às 15h” prepara e lê a proposta; nada é salvo antes de confirmar.
- Uma revisão da proposta invalida a anterior; proposta expirada ou de outra conta não é executada.
- Timeout de criação não provoca nova gravação automática; Verificar estado e nova tentativa usam o mesmo identificador.
- Cancelar reprodução durante uma criação não produz a afirmação falsa de que o evento foi desfeito.
- Conversas podem ser retomadas após reiniciar; busca por assunto e por data recupera fontes identificadas.
- Falha de download/documentação mantém a última versão utilizável.
- A UI permanece utilizável em 360, 390, 768 e 1280 px, com teclado, zoom de 200% e movimento reduzido.

## Não regressão

Preservar os dez testes originais de memória, compatibilidade e cancelamento. Foram adicionados testes de datas/fusos, migração, isolamento de conhecimento, instalação, protocolos, operações e extensibilidade. Executar a suíte apropriada por alteração:

| Alteração | Checks |
|---|---|
| Banco/migração | Reabertura de fixtures antigos, FTS, exclusão e operações |
| VAD/resampling | Corpus fixo, silêncio, início/fim da fala e buffers limitados |
| STT/TTS | Corpus pt-BR e frases de referência; anotar revisão/hardware |
| Provedor | UTF-8, stream truncado, tools, cancelamento e probe real |
| Calendar | Fusos, conta, permissões, confirmação, conflito e reconciliação |
| UI/IPC | Ponte JS, compilação WASM, canais, responsividade e estados |
| Pacote | Instalação limpa, permissões, recursos Python, offline e atualização |

A CI foi preparada para Ubuntu 24.04 com testes de domínio/runtime/Python/JS, build WASM e verificação Tauri. Sua configuração foi criada; uma execução remota dessa CI não ocorreu nesta sessão.

## Metas a medir

Na máquina de referência, após aquecimento: primeiro áudio em p95 até cinco segundos para respostas sem ferramentas; parada explícita em p95 até 200 ms; busca lexical em p95 até 150 ms com dez mil mensagens; cem turnos sem crescimento contínuo de recursos.

Essas metas não foram demonstradas. O teste de cem turnos disponível verifica descarte de callbacks cancelados, não estabilidade de cem inferências neurais reais. Registrar CPU, RAM, SO, versões e percentis antes de aprovar distribuição.

Fora do MVP: Windows/macOS, múltiplas contas simultâneas, interrupção natural com cancelamento de eco, edição/exclusão de eventos, convites, recorrências novas, Meet, Kokoro e recuperação semântica por embeddings.
