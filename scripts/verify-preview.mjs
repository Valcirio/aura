// Smoke da prévia usando Chromium/CDP e apenas módulos nativos do Node 22+.
// CHROME_BIN=/caminho/para/chrome node scripts/verify-preview.mjs
import {writeFile, mkdir, mkdtemp, rm} from 'node:fs/promises';
import {spawn} from 'node:child_process';
import {resolve} from 'node:path';
import {tmpdir} from 'node:os';
import {pathToFileURL} from 'node:url';
import assert from 'node:assert/strict';

const root = resolve(import.meta.dirname, '..');
const chrome = process.env.CHROME_BIN;
if (!chrome) throw new Error('Defina CHROME_BIN para um Chromium instalado.');
const profile = await mkdtemp(resolve(tmpdir(), 'aura-chrome-'));
const browser = spawn(chrome, [
  '--headless', '--no-sandbox', '--disable-dev-shm-usage', '--disable-gpu',
  '--remote-debugging-pipe', '--allow-file-access-from-files', `--user-data-dir=${profile}`,
  '--use-fake-device-for-media-stream', '--use-fake-ui-for-media-stream', 'about:blank',
], {stdio: ['ignore', 'ignore', 'pipe', 'pipe', 'pipe']});
let id = 0, buffer = '', diagnostics = '';
const pending = new Map(), events = [];
browser.stderr.on('data', chunk => { diagnostics += chunk.toString(); });
browser.stdio[4].on('data', chunk => {
  buffer += chunk.toString();
  let end;
  while ((end = buffer.indexOf('\0')) >= 0) {
    const raw = buffer.slice(0, end);
    buffer = buffer.slice(end + 1);
    if (!raw) continue;
    const message = JSON.parse(raw);
    if (!message.id) { events.forEach(handler => handler(message)); continue; }
    const callbacks = pending.get(message.id);
    if (!callbacks) continue;
    pending.delete(message.id);
    if (message.error) callbacks.reject(new Error(JSON.stringify(message.error)));
    else callbacks.resolve(message.result);
  }
});
const fail = error => { for (const callback of pending.values()) callback.reject(error); pending.clear(); };
browser.on('error', fail);
browser.on('exit', code => fail(new Error(`Chromium encerrou (${code}): ${diagnostics.slice(-1500)}`)));
browser.stdio[3].on('error', error => fail(new Error(`Pipe Chromium indisponível: ${error.message}`)));
browser.stdio[4].on('error', error => fail(new Error(`Pipe Chromium indisponível: ${error.message}`)));
const call = (method, params = {}, sessionId) => new Promise((resolve, reject) => {
  const requestId = ++id;
  const timer = setTimeout(() => { pending.delete(requestId); reject(new Error(`Timeout CDP: ${method}`)); }, 10000);
  pending.set(requestId, {resolve: result => { clearTimeout(timer); resolve(result); }, reject: error => { clearTimeout(timer); reject(error); }});
  browser.stdio[3].write(JSON.stringify({id: requestId, method, params, sessionId}) + '\0');
});
try {
  const {targetId} = await call('Target.createTarget', {url: 'about:blank'});
  const {sessionId} = await call('Target.attachToTarget', {targetId, flatten: true});
  const page = (method, params) => call(method, params, sessionId);
  const errors = [];
  events.push(message => {
    if (message.method === 'Runtime.exceptionThrown') errors.push(message.params.exceptionDetails.text);
  });
  const evaluate = async expression => {
    const result = await page('Runtime.evaluate', {expression, returnByValue: true, awaitPromise: true, userGesture: true});
    if (result.exceptionDetails) throw new Error(JSON.stringify(result.exceptionDetails));
    return result.result.value;
  };
  const until = async expression => {
    for (let attempt = 0; attempt < 100; attempt++) {
      if (await evaluate(expression)) return;
      await new Promise(resolve => setTimeout(resolve, 50));
    }
    throw new Error(`Timeout: ${expression}`);
  };
  await page('Runtime.enable');
  await page('Page.enable');
  await page('Page.navigate', {url: pathToFileURL(resolve(root, 'design/index.html')).href});
  await until("document.querySelector('.app')?.dataset.mounted === 'true'");
  const artifacts = resolve(root, 'artifacts');
  await mkdir(artifacts, {recursive: true});
  for (const width of [1280, 768, 390, 360]) {
    await page('Emulation.setDeviceMetricsOverride', {width, height: 1000, deviceScaleFactor: 1, mobile: false});
    await evaluate('new Promise(resolve => requestAnimationFrame(() => requestAnimationFrame(resolve)))');
    assert.equal(await evaluate('document.documentElement.scrollWidth <= innerWidth'), true, `Overflow em ${width}px`);
    const screenshot = await page('Page.captureScreenshot', {format: 'png', captureBeyondViewport: true});
    await writeFile(resolve(artifacts, `conversation-${width}.png`), Buffer.from(screenshot.data, 'base64'));
  }
  console.log('PASS: layout sem overflow em 1280, 768, 390 e 360 px');

  await evaluate("document.querySelector('[data-action=\"new-session\"]').click(); document.querySelector('[data-page=\"memory\"]').click()");
  await until("document.querySelector('#session-list').textContent.includes('Somente nesta aba')");
  assert.equal(await evaluate("document.querySelector('#memory').hidden"), false);
  await evaluate("document.querySelector('#memory-query').value = 'roteador'; document.querySelector('#memory-form').requestSubmit()");
  await until("document.querySelector('#memory-results').textContent.includes('não contém conversas reais')");
  await evaluate("document.querySelector('#session-list [aria-label^=\"Excluir\"]').click()");
  await until("document.querySelector('#session-list').textContent.includes('Ainda sem conversas')");
  console.log('PASS: navegação, criação/exclusão e distinção entre memória de prévia e real');

  await evaluate("document.querySelector('[data-page=\"settings\"]').click()");
  assert.equal(await evaluate("[...document.querySelectorAll('#settings .model-item strong')].map(el => el.textContent).join(',')"), 'Cadu,Faber');
  await evaluate("document.querySelector('[data-page=\"conversation\"]').click()");
  for (const state of ['idle', 'listening', 'transcribing', 'thinking', 'searching', 'speaking', 'error']) {
    await evaluate(`document.querySelector('[data-demo-state="${state}"]').click()`);
    assert.equal(await evaluate("document.querySelector('.app').dataset.state"), state);
    assert.equal(await evaluate("document.querySelector('#stage-description').textContent.includes('Simulação')"), true);
  }
  console.log('PASS: catálogo de vozes e sete estados explícitos de demonstração');

  await evaluate(`
    window.capturedTracks = [];
    const originalCapture = navigator.mediaDevices.getUserMedia.bind(navigator.mediaDevices);
    navigator.mediaDevices.getUserMedia = async constraints => {
      const stream = await originalCapture(constraints);
      capturedTracks.push(...stream.getTracks());
      return stream;
    };
    document.querySelector('#mic-button').click();
  `);
  await until("document.querySelector('.app').dataset.state === 'listening' && capturedTracks.length > 0");
  await evaluate("document.querySelector('#stop-button').click()");
  assert.equal(await evaluate("capturedTracks.every(track => track.readyState === 'ended')"), true);
  console.log('PASS: microfone fake captura e libera todas as faixas ao parar');

  // WAV de 3 segundos, PCM mono/16 bits/16 kHz. Não representa síntese de voz.
  await evaluate(`
    const samples = 48000, bytes = new ArrayBuffer(44 + samples * 2), view = new DataView(bytes);
    const str = (offset, text) => [...text].forEach((char, i) => view.setUint8(offset + i, char.charCodeAt(0)));
    str(0,'RIFF'); view.setUint32(4,36+samples*2,true); str(8,'WAVE'); str(12,'fmt ');
    view.setUint32(16,16,true); view.setUint16(20,1,true); view.setUint16(22,1,true);
    view.setUint32(24,16000,true); view.setUint32(28,32000,true); view.setUint16(32,2,true); view.setUint16(34,16,true);
    str(36,'data'); view.setUint32(40,samples*2,true);
    for(let i=0;i<samples;i++) view.setInt16(44+i*2,Math.sin(i*2*Math.PI*440/16000)*9000,true);
    const transfer = new DataTransfer(); transfer.items.add(new File([bytes],'teste-saida.wav',{type:'audio/wav'}));
    const input = document.querySelector('#sample-input'); input.files = transfer.files;
    input.dispatchEvent(new Event('change'));
  `);
  await until("document.querySelector('.app').dataset.state === 'speaking'");
  await until("Number(document.querySelector('.app').style.getPropertyValue('--orb-scale')) > 1.02");
  await evaluate("document.querySelector('#stop-button').click()");
  assert.equal(await evaluate("document.querySelector('.app').style.getPropertyValue('--orb-scale')"), '1');
  console.log('PASS: amplitude de áudio real altera o círculo; parada zera o nível');

  await page('Emulation.setEmulatedMedia', {features: [{name: 'prefers-reduced-motion', value: 'reduce'}]});
  await evaluate("document.querySelector('[data-demo-state=\"thinking\"]').click()");
  assert.equal(await evaluate("getComputedStyle(document.querySelector('.orbit')).animationName"), 'none');
  assert.equal(await evaluate("getComputedStyle(document.querySelector('.orb')).transform"), 'none');
  assert.deepEqual(errors, []);
  console.log('PASS: movimento reduzido e nenhuma exceção JS');
} finally {
  browser.kill('SIGTERM');
  await new Promise(resolve => { if (browser.exitCode !== null || browser.signalCode !== null) resolve(); else browser.once('exit', resolve); });
  await rm(profile, {recursive: true, force: true});
}
