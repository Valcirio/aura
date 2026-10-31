# Design implementado

A interface usa fundo marfim, verdes discretos, tipografia de sistema nos controles e serifa nos títulos. O agente é um círculo com luz e sombra, sem reutilizar personagem ou mídia do Coucou.

## Páginas

- **Conversa:** círculo, estado textual, início/parada, conclusão manual da fala, modelo/voz, transcrição, fontes e propostas de compromisso.
- **Memória:** busca de mensagens, lista paginada de sessões, abertura/retomada e exclusão.
- **Serviços:** conexão Google, conta atual, agendas, documentação e atalho para consultar compromissos.
- **Configurações:** Ollama/API, modelo/contexto, teste de compatibilidade, instalação dos motores, vozes, dispositivos, fuso e credenciais.

O estado funcional é mantido em signals Leptos. A ponte JS não controla mensagens ou navegação do aplicativo real. A prévia HTML continua separada, com seus próprios controles explícitos de demonstração.

## Voz e estados

| Estado | Feedback |
|---|---|
| Preparando | Aguarda aquecimento dos motores e abertura do dispositivo |
| Ouvindo | Texto explícito, círculo ligado ao nível de entrada e botão Concluir fala |
| Transcrevendo | Indica transformação da fala em texto |
| Pensando | Arco discreto e descrição de processamento |
| Documentação / Memória / Web | Identifica de onde o agente está buscando contexto |
| Consultando agenda | Indica acesso a dados atuais do Calendar |
| Aguardando confirmação | Proposta ainda não gravada, com campos legíveis |
| Salvando | Criação em andamento |
| Respondendo | Círculo ligado ao áudio de saída |
| Erro | Mensagem recuperável; nenhuma afirmação de sucesso fictício |

A barra superior também indica o trabalho em andamento e oferece Parar quando há conversa ativa, inclusive fora da página Conversa. A indicação de compatibilidade do modelo depende da configuração testada.

## Círculo e vozes

O RMS de saída é calculado nos frames entregues ao dispositivo e associado ao timestamp previsto de playback pelo CPAL. A ponte atualiza somente duas propriedades CSS: escala e halo. A lista de mensagens não é reconciliada a cada frame de volume. Ao parar, o nível é zerado.

Vozes do MVP: **Cadu** e **Faber**, ambas pt-BR no Piper. Instalação e amostras reais ficam em Configurações. Voz ausente não é substituída silenciosamente por outra. Kokoro/Dora/Alex/Santa continuam documentados como expansão, sem motor conectado nesta versão.

## Confirmação de Calendar

O cartão mostra título, agenda, início, término, fuso e sobreposição conhecida. Datas são apresentadas em português brasileiro; eventos de dia inteiro mostram o último dia incluído, embora a API receba término exclusivo.

A proposta não cria evento. O usuário confirma pelo botão ou por voz, por exemplo “confirmo” ou “pode salvar”. Alterações pedem nova confirmação. O cartão distingue concluído, cancelado, em execução e resultado desconhecido. Desconhecido oferece Verificar estado e Preparar nova tentativa, sem repetição automática de escrita.

## Responsividade e acessibilidade

Acima de 900 px, sidebar com nomes; entre 581 e 900 px, sidebar compacta; até 580 px, navegação inferior e cartões empilhados. A janela mínima é 360×600. Formulários e transcrições quebram conteúdo longo e podem rolar.

Há foco visível, rótulos de controles, estado textual, mensagem de status e suporte a `prefers-reduced-motion`, que desliga escala/rotação. A transcrição incremental não anuncia cada token ao leitor de tela. A auditoria de contraste, leitor de tela, zoom e WebView real ainda precisa ser realizada; não há certificação de acessibilidade nesta entrega.

## Revisão visual

`design/index.html` é uma prévia servida em localhost. Testa microfone e reprodução de um arquivo escolhido, sem enviar nem salvar áudio. Seus estados simulados não acionam modelos. O aplicativo funcional é carregado por Tauri/Trunk e não pode ser substituído pelo servidor estático Python.

O smoke visual preparado usa Chromium, microfone sintético e uma amostra WAV gerada para verificar amplitude, navegação, movimento reduzido e overflow. Sua execução continua bloqueada neste sandbox; não foram produzidas capturas verificadas da UI nativa.
