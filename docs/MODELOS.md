# Modelos locais e critérios de escolha

**Implementação atual:** Whisper `base` multilíngue, Silero VAD e Piper com Cadu/Faber, via worker CPU persistente. Kokoro é uma alternativa pesquisada, ainda sem adaptador. Os motores e pesos não puderam ser instalados neste ambiente por bloqueio de rede; não houve benchmark neural.

Pesquisa em fontes dos mantenedores, em 29/10/2026. “Leve” considera pesos, runtime, RAM de pico e velocidade no computador de destino. O host inspecionado tem i7-11800H e 32 GB de RAM; a implementação usa CPU e esses dados não garantem latência.

## Transcrição — STT

| Candidato | Pesos aproximados | RAM publicada pelo whisper.cpp | Aplicação proposta |
| --- | --- | --- | --- |
| Whisper `tiny` multilíngue | 75 MiB | ~273 MB | Máquinas limitadas; comparar erros em português |
| Whisper `base` multilíngue | 142 MiB | ~388 MB | Padrão inicial candidato |
| Whisper `small` multilíngue | 466 MiB | ~852 MB | Alternativa quando `base` errar termos de suporte |

Os valores de RAM são referências do projeto, não o consumo total do aplicativo. O runtime oferece quantização e aceleração dependente da plataforma. Evitar variantes `.en` no produto em português. Licença do whisper.cpp: MIT. Fontes: [whisper.cpp](https://github.com/ggml-org/whisper.cpp), [pesos GGML e quantizações](https://huggingface.co/ggerganov/whisper.cpp).

Opção de download menor: `base-q5_1`, listado com aproximadamente 57 MiB no catálogo do mantenedor. Avaliar perda de qualidade antes de torná-lo padrão. O modelo precisa estar aquecido durante uma conversa; carregar pesos a cada turno aumenta a latência.

### Detecção de atividade vocal

Silero VAD é um componente separado da transcrição. Usar uma versão ONNX fixada e validar janelas/frequências aceitas por essa versão; o processamento será feito em mono a 16 kHz. VAD não é cancelamento de eco nem transcrição. Fonte: [Silero VAD](https://github.com/snakers4/silero-vad).

## Geração de fala — TTS

| Candidato | Perfil | Português / vozes | Decisão |
| --- | --- | --- | --- |
| Piper | Motor local com vozes ONNX independentes | Catálogo pt-BR inclui famílias `cadu`, `edresson`, `faber`, `jeff` | Primeiro candidato para CPU e distribuição enxuta |
| Kokoro 82M | 82 milhões de parâmetros; pesos Apache-2.0 | `pf_dora`, `pm_alex`, `pm_santa`; `lang_code='p'` | Alternativa com três opções pt-BR no mesmo motor |

Piper exige modelo `.onnx` e configuração `.onnx.json`. O tamanho varia por voz/qualidade; registrar o tamanho exato do artefato escolhido no manifesto, em vez de atribuir um tamanho único ao motor. O motor atual é mantido em `OHF-Voice/piper1-gpl`; sua licença é GPL-3.0. As vozes têm licenças próprias em cada `MODEL_CARD`. Rever os requisitos de distribuição antes do empacotamento. Fontes: [Piper](https://github.com/OHF-Voice/piper1-gpl), [formato das vozes](https://github.com/OHF-Voice/piper1-gpl/blob/main/docs/VOICES.md), [catálogo pt-BR](https://huggingface.co/rhasspy/piper-voices/tree/main/pt/pt_BR).

No Kokoro, 82M parâmetros não significam download de 82 MB: representação numérica, vozes e runtime mudam o tamanho. O catálogo oficial identifica Dora, Alex e Santa como vozes brasileiras. Avaliar naturalidade com números, URLs e termos técnicos. O mantenedor aponta limitações em idiomas com menos dados e frases muito curtas/longas. Fontes: [modelo](https://huggingface.co/hexgrad/Kokoro-82M), [catálogo de vozes](https://huggingface.co/hexgrad/Kokoro-82M/blob/main/VOICES.md), [runtime e idiomas](https://github.com/hexgrad/kokoro).

Não exigir clonagem de voz no MVP. O seletor expõe somente vozes instaladas e compatíveis com o motor ativo. Uma voz não instalada aparece com ação de download; nunca produzir a voz padrão silenciosamente após o usuário selecionar outra.

## LLM local ou remoto

O LLM não precisa ser de áudio. Candidato local: Qwen3 de pequeno porte via Ollama; começar o ensaio com a família de 4B parâmetros, reduzindo contexto e avaliando quantização conforme RAM disponível. O Qwen3-4B-Instruct-2507 possui pesos Apache-2.0, mas a compatibilidade do artefato servido deve ser testada. Fontes: [model card Qwen](https://huggingface.co/Qwen/Qwen3-4B-Instruct-2507), [tool calling Ollama](https://docs.ollama.com/capabilities/tool-calling).

Estimativa de engenharia: pesos de um modelo 4B a 4 bits têm ordem de grandeza de 2 GB antes de metadados, runtime e KV cache; não é estimativa de RAM total. Máquinas de 8 GB devem primeiro testar LLM remoto com STT/TTS locais. Para um fluxo totalmente local com 4B, 16 GB é um ponto de partida para avaliação, sem garantia universal.

Conector remoto inicial: API compatível com Chat Completions, configurada pelo usuário. Modelos e preços devem ser consultados no provedor escolhido durante a implementação. Não fixar uma lista comercial como se todos os endpoints tivessem as mesmas capacidades.

## Benchmark para decidir o padrão

Conjunto inicial: 40 gravações pt-BR com transcrições revisadas, combinando sotaques, voz baixa, ruído moderado, nomes, números, termos de rede e pausas. Medir WER por grupo, real-time factor, duração do carregamento e RAM de pico. Manter as mesmas gravações para todas as variantes; não usar somente inglês nem somente a voz do desenvolvedor.

Para TTS: 20 frases de suporte, incluindo abreviações, versões, valores e links. Medir tempo até primeiro PCM, duração de geração/duração de áudio, falhas e avaliação de inteligibilidade por pelo menos três ouvintes. Gravar os resultados com CPU, RAM, SO, modelo, revisão, backend e aquecimento.

Metas iniciais propostas: STT com real-time factor < 0,5 em falas de 10 s, TTS < 1,0, sem travamento perceptível de UI. Os limiares de WER serão aprovados a partir da comparação no corpus; nenhum resultado foi medido nesta entrega. Ver metas de experiência e regressão em [PLANO.md](PLANO.md).
