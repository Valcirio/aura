# Contratos de integração

## Transporte

O command Tauri `app_command` recebe `{command, args}` e responde sem eventos intermediários. O command `app_stream` recebe `{command, args, onEvent}`, onde `onEvent` é um `Channel<Value>` Tauri (obrigatório, não um objeto arbitrário serializado pela aplicação) que entrega eventos tipados antes da resposta final. `command` identifica uma operação da lista fechada abaixo; `args` usa snake_case e é validado por structs com campos desconhecidos recusados.

Exemplo da ponte JS:

```javascript
const response = await window.__TAURI__.core.invoke('app_command', {
  command: 'memory_search',
  args: {query: 'reunião', session_id: null}
});
```

Operações com eventos usam `app_stream` com `onEvent: new window.__TAURI__.core.Channel()`.

A ponte Leptos encapsula essa chamada. O harness `aura-runtime` usa o mesmo dispatcher em NDJSON: `{id,command,args}` → `{id,result}` ou `{id,error}`. Não há servidor HTTP permanente. O listener loopback do OAuth existe somente durante autorização.

## Operações

| Grupo | Command | Entrada principal |
|---|---|---|
| Configuração | `settings_get` | `{}` |
| Configuração | `settings_save` | `settings`, chaves opcionais `provider_key`, `brave_key`, `google_secret` |
| Modelos | `provider_models`, `provider_probe`, `provider_check`, `provider_status` | `{}`; usam a configuração salva |
| Sessões | `session_create` | `title` |
| Sessões | `session_list` | `before?` para paginação de 30 sessões |
| Sessões | `session_messages` | `session_id`, `after?`; até 30 mensagens |
| Sessões | `session_delete` | `session_id` |
| Memória | `memory_search` | `query`, `session_id?`; até 8 resultados |
| Conversa | `conversation_start` | `session_id`, `text?`; sem texto inicia voz automática; channel obrigatório |
| Conversa | `conversation_finish_input`, `conversation_cancel` | `{}` |
| Integrações | `integration_list`, `integration_status` | `{}`; a lista retorna o manifesto mais `connected`, `enabled`, `connectable` e `tools` (`[{name,label,effect}]`) |
| Integrações | `integration_connect`, `integration_disconnect` | `integration` (alias legado `service_id`); só `google_calendar` é conectável, as demais retornam `SERVICE_NOT_CONNECTABLE` |
| Integrações | `integration_set_enabled` | `integration`, `enabled`; atualiza `settings.integrations_disabled` e devolve a lista resultante |
| Conhecimento | `knowledge_refresh` | `integration` (alias legado `service_id`) |
| Conhecimento | `knowledge_search` | `query` |
| Fontes | `source_open` | `url`; HTTP/HTTPS sem credenciais, aberto no navegador do sistema |
| Calendar | `calendar_list` | `{}` |
| Calendar | `calendar_events` | `calendar_id`, `start`, `end`, `query?`, `page_token?` |
| Calendar | `calendar_free_busy` | `calendar_ids`, `start`, `end` |
| Operação | `action_get` | `session_id`; última operação, inclusive concluída |
| Operação | `action_confirm`, `action_cancel`, `action_retry` | `id`, `session_id` |
| Operação | `action_reconcile` | `id` |
| Áudio | `audio_devices`, `model_status`, `voice_list` | `{}` |
| Áudio | `speech_prepare`, `speech_status` | `{}`; preparação em segundo plano e snapshot em cache |
| Áudio | `alert_notice` | `kind`: `provider`, `models`, `capture` ou `synthesis` |
| Áudio | `model_install` | `id` do catálogo; channel opcional de progresso |
| Áudio | `voice_preview` | `id` da voz instalada |

`service_list`, `service_status`, `service_connect` e `service_disconnect` são aliases **obsoletos** dos handlers `integration_*`.

Selecionar modelo é salvar a configuração e aprová-la em `provider_probe`; não há uma segunda seleção silenciosa. Os commands antigos de sessões, memória e `model_validate` continuam como aliases diretos Tauri para compatibilidade. `model_validate` continua sendo somente validação de flags; o aplicativo real usa o probe.

Campos de credencial ausentes ou `null` preservam o segredo. Uma string vazia fornecida explicitamente ao backend remove o valor; a UI trata seu campo vazio como preservação. O retorno de settings não inclui segredos.

## Contratos de verificação do provedor, fala e ponte

- **`provider_check`** (sem argumentos) retorna o probe armazenado quando ainda é válido para a configuração atual; caso contrário executa o probe completo e o publica. Chamadas concorrentes para a mesma revisão da configuração compartilham uma única operação, limitada a 60 s; ao expirar retorna `PROVIDER_TIMEOUT` (`retryable`). `provider_status` devolve o último probe publicado ou `null`.
- **`speech_prepare`** inicia no máximo uma preparação em segundo plano dos modelos nativos selecionados (ASR, VAD e TTS, nessa ordem, seguidos da pré-carga real) e retorna imediatamente o snapshot. Estado pronto ou em andamento é devolvido sem novo trabalho; após falha, nova chamada tenta de novo. **`speech_status`** só lê o snapshot em cache (`phase`, `ready`, `active`, `artifact`, `file`, `downloaded`, `total`, `error`, `code`, `warning`, `generation`), sem hash, disco ou rede. Sem a feature `local-speech` o snapshot é `unsupported` (`SPEECH_UNSUPPORTED`).
- **Instalação com origem local verificada:** cada arquivo do catálogo é reutilizado se a instalação existente ou a cópia em `AURA_MODEL_SOURCE_ROOT/<id>/<arquivo>` (padrão: `tools/audio-bench/data/models`, quando existe) confere em tamanho e SHA-256; só então ocorre o download HTTPS fixado. Arquivo local ausente ou inválido, portanto, pode iniciar download. A origem local é copiada, nunca movida.
- **`alert_notice`** reproduz um dos quatro avisos WAV pt-BR embutidos no binário (sem TTS e sem modelos); o chamador só escolhe a variante, sem texto, caminho ou áudio. Um novo aviso cancela o anterior e a reprodução é serializada com a fala.
- **Retenção do canal IPC (`public/bridge.js`):** cada `requestStream` reserva o canal por `id`; id em uso é recusado com `STREAM_IN_USE`. Em `conversation_start` o canal é retido até `run.completed`/`turn.cancelled`, falha ou descarte (`disposeStream`); eventos anteriores ao ticket são guardados e reproduzidos em ordem apenas se `session_id` e `generation` coincidirem com o ticket (inválido: `INVALID_TICKET`). Nos demais commands o canal é liberado quando a chamada termina.

## Eventos

```json
{
  "session_id": 3,
  "turn_id": "id-do-turno",
  "generation": 8,
  "sequence": 27,
  "kind": "state.changed",
  "payload": {"state": "consulting"}
}
```

Tipos emitidos: `run.started`, `state.changed`, `transcript.final`, `tool.started`, `tool.completed`, `sources`, `assistant.delta`, `audio.level`, `audio.error`, `action.changed`, `turn.completed`, `turn.cancelled`, `error`, `run.completed`, `model.progress`.

`tool.started` tem payload `{"name","label","effect"}`: `effect` é `read`/`external_write`/… (snake_case de `Effect`), ou `null` quando a ferramenta é desconhecida. `tool.completed` tem payload `{"name","label","ok"}`. `assistant.delta` chega de todas as rodadas do turno, não só da resposta final.

O loop do agente, por turno, faz no máximo 8 rodadas com ferramentas mais 1 rodada final sem ferramentas; no máximo 12 chamadas de ferramenta (acima disso, `TOOL_BUDGET`); e no máximo 1 ação proposta (uma segunda chamada que exige aprovação na mesma rodada resulta em `ACTION_AMBIGUOUS`). Depois que uma ação é proposta, a rodada seguinte não oferece ferramentas e apenas pede confirmação. Rodadas somente de leitura executam até 2 chamadas ao mesmo tempo; rodadas com escrita executam em sequência.

`audio.level` contém `rms` e origem `input`/`output`; o backend já considera o clock de reprodução. A UI ignora gerações e épocas locais obsoletas. A conclusão de um turno espera o stream final e o esvaziamento da fila de áudio. Cancelamento de uma conversa não desfaz operações externas confirmadas.

## Eventos do Calendar e confirmação

```json
{
  "calendar_id": "primary",
  "summary": "Revisão do projeto",
  "start": "2026-10-30T14:00:00",
  "end": "2026-10-30T15:00:00",
  "time_zone": "America/Bahia",
  "all_day": false,
  "description": ""
}
```

Para eventos com horário, aceitam-se timestamps com offset ou horários civis resolvidos pelo fuso IANA. Horários ambíguos/inexistentes exigem esclarecimento. Para dia inteiro, `start` e `end` são datas ISO e `end` é exclusivo.

Uma operação (ação pendente) é genérica e tem os campos: `id`, `session_id`, `integration` (aceita o alias legado `service_id` na leitura), `account_id`, `tool`, `effect` (`read`|`local_write`|`external_write`|`destructive`|`system`), `summary`, `fields` (`[{label,value}]`, valores brutos; a UI formata), `payload` (JSON específico da integração; para `calendar_create_event` é o `EventDraft` acima), `status`, `expires_at` e `result` (no calendário, disponibilidade `{busy:[...]}`). Nenhuma criação ocorre nessa etapa. Estados: `awaiting_confirmation`, `executing`, `completed`, `unknown`, `failed`, `cancelled`, `expired`. O schema SQLite v2 adiciona as colunas `tool`, `effect`, `summary` e `fields`; a coluna antiga `event` guarda o payload.

`action_confirm` faz claim atômico, verifica conta/sessão, revalida disponibilidade e cria no Google. Chamadas repetidas a uma operação concluída retornam seu resultado. Uma proposta substituída, expirada ou de outra conta não pode ser gravada. `action_retry` somente reabre uma operação desconhecida quando o mesmo evento não foi encontrado; não faz a gravação e exige nova confirmação.

O LLM recebe apenas ferramentas de consulta e `calendar_create_event`, que só propõe; a execução exige confirmação do usuário. `action_confirm` pertence ao controle de UI/voz do aplicativo e não consta do catálogo de ferramentas do modelo.

## Ferramentas comuns e erros

`memory_search` do agente aceita `query`, `since?`, `until?`. Uma query vazia é permitida quando há filtro temporal. `conversation_get` recebe sessão e cursor; `knowledge_search` recebe query e `integration?` opcional; pesquisa todas as integrações por padrão. `web_search` recebe query pública e recência opcional `pd`, `pw`, `pm` ou `py`.

Erros possuem `{code,message,retryable}`. Entre os códigos: `AUTH_REQUIRED`, `OAUTH_DENIED`, `OAUTH_TIMEOUT`, `MODEL_NOT_VERIFIED`, `MODEL_INCOMPATIBLE`, `STREAM_TRUNCATED`, `CONTEXT_LIMIT`, `ACTION_NOT_CONFIRMABLE`, `AVAILABILITY_CHANGED`, `DEVICE_UNAVAILABLE`, `AUDIO_DEPENDENCIES`, `MODEL_NOT_INSTALLED`, `CANCELLED` e `COMMAND_NOT_ALLOWED`.

Respostas HTTP e erros de banco não são expostos integralmente à UI. Fontes carregam IDs e metadados; resultados grandes são limitados e podem ser reduzidos no contexto enviado ao modelo. O cliente deve comunicar resultados desconhecidos sem convertê-los em sucesso ou em cancelamento garantido.
