// Serviços da prévia: medidor real de áudio e demonstração dos commands de memória.
// Não faz STT, não chama um LLM e não simula resultados de pesquisa web.
const descriptions = {
  idle: ['Pronta para começar', 'Como posso ajudar?', 'Teste a entrada de áudio ou ouça uma amostra gerada por uma voz local.'],
  listening: ['Ouvindo', 'Pode falar. Estou ouvindo.', 'Teste de microfone: o círculo acompanha o volume real da sua voz.'],
  transcribing: ['Transcrevendo · simulação', 'Transformando voz em palavras.', 'Prévia visual. O motor de transcrição ainda não está conectado.'],
  thinking: ['Pensando · simulação', 'Um instante para entender.', 'Prévia visual do processamento da resposta.'],
  searching: ['Pesquisando · simulação', 'Buscando o contexto certo.', 'Prévia visual da busca na web ou nas conversas anteriores.'],
  speaking: ['Reproduzindo áudio', 'Uma resposta para ouvir.', 'O círculo acompanha a amplitude do arquivo local em reprodução.'],
  error: ['Algo precisa de atenção', 'Vamos tentar de novo?', 'Verifique o dispositivo, a permissão de microfone ou o formato da amostra.'],
};

export function mountInteractions() {
  requestAnimationFrame(() => {
    const app = document.querySelector('.app');
    if (!app || app.dataset.mounted) return;
    app.dataset.mounted = 'true';
    let context, stream, source, animation, generation = 0;
    let previewSessions = [];
    const native = window.__TAURI__?.core;
    const byId = id => document.getElementById(id);
    const invoke = (command, args = {}) => native.invoke(command, args);

    function state(name, detail) {
      app.dataset.state = name;
      const labels = descriptions[name];
      byId('state-label').textContent = labels[0];
      byId('stage-title').textContent = labels[1];
      byId('stage-description').textContent = detail || labels[2];
    }
    function stop() {
      generation++;
      cancelAnimationFrame(animation);
      stream?.getTracks().forEach(track => track.stop());
      stream = undefined;
      if (source) { try { source.stop?.(); source.disconnect(); } catch {} }
      source = undefined;
      context?.close().catch(() => {});
      context = undefined;
      app.style.setProperty('--orb-scale', '1');
      app.style.setProperty('--orb-glow', '.18');
      byId('mic-button').setAttribute('aria-pressed', 'false');
      byId('mic-label').textContent = 'Testar microfone';
      state('idle');
    }
    function meter(analyser, epoch) {
      const samples = new Float32Array(analyser.fftSize);
      let smoothed = 0;
      function frame() {
        if (generation !== epoch) return;
        analyser.getFloatTimeDomainData(samples);
        const rms = Math.sqrt(samples.reduce((sum, sample) => sum + sample * sample, 0) / samples.length);
        smoothed = .65 * smoothed + .35 * Math.min(1, rms * 4);
        app.style.setProperty('--orb-scale', String(1 + smoothed * .22));
        app.style.setProperty('--orb-glow', String(.18 + smoothed * .5));
        animation = requestAnimationFrame(frame);
      }
      frame();
    }
    byId('mic-button').addEventListener('click', async () => {
      if (stream) { stop(); return; }
      stop();
      const epoch = generation;
      let pendingContext;
      try {
        if (!navigator.mediaDevices?.getUserMedia) throw new Error('Microfone requer localhost ou um contexto seguro compatível.');
        pendingContext = new AudioContext();
        context = pendingContext;
        await pendingContext.resume();
        if (epoch !== generation) return;
        const captured = await navigator.mediaDevices.getUserMedia({audio: {echoCancellation: true, noiseSuppression: true}, video: false});
        if (epoch !== generation) { captured.getTracks().forEach(t => t.stop()); pendingContext.close().catch(() => {}); return; }
        stream = captured;
        const analyser = pendingContext.createAnalyser();
        analyser.fftSize = 1024;
        source = pendingContext.createMediaStreamSource(captured);
        source.connect(analyser); // Sem retorno ao alto-falante: evita realimentação.
        state('listening');
        byId('mic-button').setAttribute('aria-pressed', 'true');
        byId('mic-label').textContent = 'Encerrar teste';
        meter(analyser, epoch);
      } catch (error) {
        if (epoch !== generation) return;
        stop();
        state('error', `Microfone indisponível: ${error.message}`);
      }
    });
    byId('sample-button').addEventListener('click', () => byId('sample-input').click());
    byId('sample-input').addEventListener('change', async event => {
      const file = event.target.files[0];
      event.target.value = '';
      if (!file) return;
      stop();
      const epoch = generation;
      if (file.size > 25 * 1024 * 1024) { state('error', 'Escolha uma amostra de até 25 MB.'); return; }
      try {
        const playbackContext = new AudioContext();
        context = playbackContext;
        await playbackContext.resume();
        const decoded = await playbackContext.decodeAudioData(await file.arrayBuffer());
        if (epoch !== generation) return;
        const analyser = playbackContext.createAnalyser();
        analyser.fftSize = 1024;
        source = playbackContext.createBufferSource();
        source.buffer = decoded;
        source.connect(analyser);
        analyser.connect(playbackContext.destination);
        source.onended = () => { if (epoch === generation) stop(); };
        state('speaking', `Amostra local: ${file.name}. Este áudio não foi gerado pelo agente.`);
        source.start();
        meter(analyser, epoch);
      } catch (error) {
        if (epoch !== generation) return;
        stop();
        state('error', `Não foi possível reproduzir esta amostra: ${error.message}`);
      }
    });
    byId('stop-button').addEventListener('click', stop);
    document.addEventListener('visibilitychange', () => { if (document.hidden) stop(); });
    window.addEventListener('pagehide', stop);
    document.querySelectorAll('[data-demo-state]').forEach(button => button.addEventListener('click', () => {
      stop();
      state(button.dataset.demoState, 'Simulação visual de estado. Nenhuma captura, geração ou busca está em execução.');
      byId('state-label').textContent += ' · prévia';
    }));

    function line(container, title, detail) {
      const element = document.createElement('div');
      element.className = 'result';
      const heading = document.createElement('strong');
      heading.textContent = title;
      const text = document.createElement('small');
      text.textContent = detail;
      element.append(heading, text);
      container.append(element);
      return element;
    }
    async function refresh() {
      const list = byId('session-list');
      list.textContent = '';
      try {
        const sessions = native ? await invoke('session_list') : previewSessions;
        if (!sessions.length) line(list, 'Ainda sem conversas', 'Crie uma sessão para testar a memória. O fluxo de voz será conectado nas próximas etapas.');
        for (const session of sessions) {
          const item = line(list, session.title, `Sessão ${session.id} · ${session.created_at}`);
          const open = document.createElement('button');
          open.className = 'text-button';
          open.textContent = 'Abrir sessão';
          open.addEventListener('click', async () => {
            const output = byId('memory-results');
            output.textContent = '';
            try {
              const messages = native ? await invoke('session_messages', {sessionId: session.id}) : [];
              line(output, session.title, messages.length ? `${messages.length} mensagens` : 'Esta sessão ainda não contém mensagens.');
              messages.forEach(message => line(output, message.role, message.text));
            } catch { line(output, 'Não foi possível abrir a sessão', 'Tente novamente.'); }
          });
          const remove = document.createElement('button');
          remove.className = 'text-button';
          remove.style.marginLeft = '20px';
          remove.textContent = 'Excluir';
          remove.setAttribute('aria-label', `Excluir sessão ${session.title}`);
          remove.addEventListener('click', async () => {
            try {
              if (native) await invoke('session_delete', {sessionId: session.id});
              else previewSessions = previewSessions.filter(s => s.id !== session.id);
              byId('memory-results').textContent = '';
              await refresh();
            } catch { line(list, 'Não foi possível excluir', 'Tente novamente.'); }
          });
          item.append(open, remove);
        }
      } catch { line(list, 'Memória indisponível', 'Não foi possível acessar as sessões locais.'); }
    }
    document.querySelectorAll('[data-action="new-session"]').forEach(button => button.addEventListener('click', async () => {
      stop();
      try {
        const title = `Conversa · ${new Date().toLocaleString('pt-BR')}`;
        if (native) await invoke('session_create', {title});
        else previewSessions.unshift({id: Date.now(), title, created_at: 'Somente nesta aba'});
        await refresh();
        state('idle', 'Sessão criada. Abra Memória para visualizá-la.');
      } catch { state('error', 'Não foi possível criar a sessão.'); }
    }));
    document.querySelector('[data-action="refresh-sessions"]').addEventListener('click', refresh);
    byId('memory-form').addEventListener('submit', async event => {
      event.preventDefault();
      const output = byId('memory-results');
      output.textContent = '';
      const query = byId('memory-query').value.trim();
      if (!query) { line(output, 'Digite algo para buscar', 'Use um assunto discutido anteriormente.'); return; }
      if (!native) { line(output, 'Busca disponível no aplicativo Tauri', 'A prévia no navegador não contém conversas reais.'); return; }
      try {
        const messages = await invoke('memory_search', {query, sessionId: null, limit: 8});
        if (!messages.length) line(output, 'Nenhum trecho encontrado', 'Tente outras palavras ou consulte a lista de sessões.');
        messages.forEach(message => line(output, `Conversa ${message.session_id} · mensagem ${message.id}`, message.text));
      } catch { line(output, 'Não foi possível buscar', 'Tente novamente.'); }
    });
    if (native) byId('runtime-label').textContent = 'Tauri · memória SQLite local';
    else byId('memory-description').textContent = 'Na prévia, sessões existem somente nesta aba. No Tauri, ficam no SQLite local.';
    refresh();
    // Navegação da versão HTML de revisão. Na aplicação, Leptos controla os painéis.
    if (document.body.dataset.preview) {
      document.querySelectorAll('[data-page]').forEach(button => button.addEventListener('click', () => {
        document.querySelectorAll('.page').forEach(panel => { panel.hidden = panel.id !== button.dataset.page; });
        document.querySelectorAll('[data-page]').forEach(item => {
          item.classList.toggle('active', item === button);
          item.setAttribute('aria-current', item === button ? 'page' : 'false');
        });
      }));
    }
  });
}

if (typeof document !== 'undefined' && document.body?.dataset.preview) mountInteractions();
