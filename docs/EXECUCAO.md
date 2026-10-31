# Executar e configurar o Aura

## 1. Preparar o Linux

Instale as dependências nativas descritas na [documentação Tauri](https://v2.tauri.app/start/prerequisites/). No Ubuntu 24.04, os pacotes de desenvolvimento incluem `libwebkit2gtk-4.1-dev`, `libgtk-3-dev`, `libasound2-dev`, `librsvg2-dev`, `patchelf`, além de compilador C/C++, pkg-config e CMake. Para credenciais persistentes, instale `libsecret-tools` e tenha um Secret Service ativo na sessão desktop. Em outras distribuições, use os pacotes equivalentes.

Na raiz do projeto:

```bash
rustup target add wasm32-unknown-unknown
cargo install trunk --locked --root .tools
cargo install tauri-cli --version '^2' --locked --root .tools
bash scripts/setup-audio.sh
bash scripts/dev.sh
```

`setup-audio.sh` requer uv. Ele baixa um Python gerenciado para `.runtime/`, copia o runtime portátil para `workers/python/`, resolve um lockfile com hashes e instala os pacotes em `workers/python-packages/`. Esses diretórios são ignorados pelo Git; o lockfile gerado deve acompanhar o build usado para distribuição.

O worker usa `numpy`, `onnxruntime`, `piper-tts` e `pywhispercpp`. Não requer PyTorch nem GPU. O modelo Whisper é multilíngue e a transcrição usa português. O VAD trabalha em frames de 512 amostras a 16 kHz. O microfone permanece fechado enquanto o agente responde.

## 2. Instalar modelos e escolher o LLM

Em Configurações, instale:

- Whisper base;
- Silero VAD;
- Cadu e/ou Faber, vozes brasileiras Piper.

Cada instalação resolve uma revisão imutável da fonte, verifica o hash de origem, grava o SHA-256 no manifesto e só ativa a revisão quando todos os arquivos estão completos. Uma instalação malsucedida preserva a revisão anterior. Os modelos ficam em `app_data_dir()/models/<artefato>/<revisão>/`, com um ponteiro `current.json` por artefato. O primeiro carregamento verifica os arquivos antes da inferência.

Para LLM local, instale Ollama e obtenha um modelo com ferramentas:

```bash
ollama pull qwen3:4b
```

Na interface, escolha Ollama, endereço `http://127.0.0.1:11434`, nome do modelo e contexto mínimo de 8192 tokens. Para uma API compatível, informe o endereço base que termina em `/v1`, quando exigido pelo provedor, e sua chave. O aplicativo acrescenta `models` e `chat/completions` a esse endereço.

Salve e execute **Testar compatibilidade**. O teste chama uma ferramenta sem efeitos externos com um nonce, verifica a resposta em português e interrompe um stream de teste. Esse é um smoke de capacidades; qualidade de linguagem e áudio exige avaliação com o corpus do projeto. O teste por API pode consumir uma pequena quantidade de tokens.

Configurações de provedores e contexto são públicas; chaves não são devolvidas à WebView. Campos de chave em branco preservam o valor atual. Sem cofre acessível, os novos segredos permanecem somente na RAM até encerrar o aplicativo.

## 3. Configurar Google Cloud

No [Google Cloud Console](https://console.cloud.google.com/):

1. Crie ou escolha um projeto e habilite a **Google Calendar API**.
2. Configure a tela de consentimento OAuth. Durante o desenvolvimento, inclua a conta que usará o aplicativo entre os usuários de teste, quando aplicável.
3. Crie um cliente OAuth do tipo **Desktop app**.
4. Copie o client ID e o client secret desse cliente para Configurações do Aura e salve. Não use credenciais de aplicação Web nem conta de serviço neste fluxo.
5. Abra Serviços e selecione **Conectar conta Google**. Conclua a autorização no navegador do sistema.

O aplicativo inicia um listener temporário em `127.0.0.1` com porta disponível, usa PKCE S256 e valida `state`. O listener existe somente durante o login e tem timeout de três minutos. Não é necessário publicar um servidor na internet nem registrar callback no localhost:8080 da prévia. O fluxo segue a [referência oficial de apps desktop](https://developers.google.com/identity/protocols/oauth2/native-app).

São solicitados `calendar.calendarlist.readonly`, `calendar.events` e `calendar.events.freebusy`. O escopo de eventos admite operações adicionais no Google; o aplicativo expõe somente consulta e criação no MVP. Alteração, exclusão, convites, recorrências novas e Meet não estão disponíveis. Consulte os [escopos oficiais](https://developers.google.com/workspace/calendar/api/auth).

Uma conta pode estar conectada por vez. Para trocar, desconecte e autorize outra. Desconectar limpa a conexão local e tenta revogar o token remoto. Sem rede, a revogação remota pode não ser confirmada, mas a conexão local é removida. Uma operação já enviada não é desfeita ao desligar o microfone ou desconectar.

## 4. Usar Calendar por voz

Carregue as agendas em Serviços e escolha a padrão. Agendas somente de leitura podem ser consultadas, mas não aceitam criação. Atualize a documentação oficial para enriquecer a base local; se uma fonte falhar, o conteúdo anterior permanece disponível.

Exemplos:

- “Quais compromissos tenho amanhã?”
- “Estou livre amanhã das duas às cinco da tarde?”
- “Crie uma revisão do projeto amanhã das 14h às 15h.”
- “O que conversamos sobre essa reunião ontem?”

O agente pergunta pelos dados ausentes. Datas relativas usam o fuso do aplicativo, exibido em Configurações. Horários locais ambíguos ou inexistentes devido a mudança de fuso são recusados até serem esclarecidos. Eventos de dia inteiro usam internamente término exclusivo, apresentado na interface como último dia incluído.

Ao preparar um evento, o aplicativo consulta a disponibilidade, mostra e lê a proposta e aguarda confirmação. Você pode dizer “confirmo” ou “pode salvar”, ou usar o botão. Uma revisão da proposta cancela a anterior. Propostas expiram em dez minutos. Se a disponibilidade mudar antes da confirmação, o aplicativo apresenta o conflito atualizado e pede confirmação novamente.

Em timeout, o resultado pode ficar **desconhecido**. Use Verificar estado. O aplicativo procura o mesmo identificador e não repete automaticamente a criação. Se o evento não for encontrado, Preparar nova tentativa reaproveita o identificador e exige outra confirmação.

## 5. Web, memória e dados

A pesquisa pública usa uma chave Brave Search opcional. Sem essa chave, o agente comunica que a ferramenta está indisponível e continua com documentação e ferramentas locais/Calendar disponíveis. O acesso ao Calendar sempre precisa de internet, mesmo com o LLM local.

Conversas, documentos e operações ficam em tabelas separadas do SQLite. O banco não é criptografado. Áudio bruto não é persistido. Recuperação de conversa usa busca lexical sem acentos e filtros temporais; não há embeddings no MVP. Históricos antigos são preservados pela migração e continuam como conversas gerais.

A exclusão de uma conversa remove suas mensagens e índices. Operações externas já realizadas permanecem registradas sem vínculo à conversa excluída; a exclusão do histórico não apaga eventos no Google Calendar.

## 6. Distribuir

```bash
bash scripts/build-linux.sh
```

O build inclui worker, Python portátil e pacotes Python instalados localmente, além dos arquivos de licença desses pacotes. Pesos são baixados pelo usuário após instalar. A versão de dependências resolvida em `workers/requirements.lock` deve ser preservada com o pacote.

A primeira distribuição deve ser validada no mesmo alvo Linux/ABI usado para build, com instalação limpa, microfone, saída de áudio, Secret Service e OAuth reais. O script gera os formatos configurados no Tauri; não há declaração de suporte a todas as distribuições Linux nem a Windows/macOS nesta entrega.
