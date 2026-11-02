// Ponte de transporte somente. Estado, navegação e interface pertencem ao Leptos.
export function isDesktop() { return Boolean(window.__TAURI__?.core); }
function core() {
  if (!isDesktop()) throw {code: 'DESKTOP_REQUIRED', message: 'Abra o aplicativo com cargo tauri dev. O servidor Python mostra apenas a prévia visual.'};
  return window.__TAURI__.core;
}
export async function request(command, argsJson) {
  try { return JSON.stringify(await core().invoke('app_command', {command, args: JSON.parse(argsJson)})); }
  catch (error) { throw JSON.stringify(typeof error === 'object' ? error : {message: String(error)}); }
}
const streams = new Map();
const TERMINAL = new Set(['run.completed', 'turn.cancelled']);
function release(id, entry) {
  entry.disposed = true;
  entry.buffer.length = 0;
  entry.channel.onmessage = () => {};
  if (streams.get(id) === entry) streams.delete(id);
}
export function disposeStream(id) {
  const entry = streams.get(id);
  if (entry) release(id, entry);
}
export async function requestStream(command, argsJson, callback, id) {
  let entry = null;
  try {
    const api = core();
    if (streams.has(id)) throw {code: 'STREAM_IN_USE', message: 'Canal de eventos já está em uso'};
    const args = JSON.parse(argsJson);
    const conversation = command === 'conversation_start';
    const channel = new api.Channel();
    const own = {channel, disposed: false, buffer: [], ticket: null};
    entry = own;
    streams.set(id, own);
    const deliver = event => {
      if (own.disposed) return;
      callback(JSON.stringify(event));
      if (conversation && TERMINAL.has(event.kind)) release(id, own);
    };
    const matches = event => event !== null && typeof event === 'object'
      && event.session_id === args.session_id && event.generation === own.ticket.generation;
    channel.onmessage = event => {
      if (own.disposed) return;
      try {
        if (!conversation) deliver(event);
        else if (own.ticket === null) own.buffer.push(event);
        else if (matches(event)) deliver(event);
      } catch { release(id, own); }
    };
    const result = await api.invoke('app_stream', {command, args, onEvent: channel});
    if (own.disposed) return JSON.stringify(result);
    if (conversation) {
      const ticket = result;
      if (ticket === null || typeof ticket !== 'object' || !Number.isInteger(ticket.session_id)
        || ticket.session_id !== args.session_id || !Number.isInteger(ticket.generation) || ticket.generation <= 0) {
        throw {code: 'INVALID_TICKET', message: 'Resposta de início inválida'};
      }
      own.ticket = ticket;
      const pending = own.buffer.splice(0);
      for (const event of pending) {
        if (own.disposed) break;
        if (matches(event)) deliver(event);
      }
    }
    return JSON.stringify(result);
  } catch (error) {
    if (entry) release(id, entry);
    throw JSON.stringify(typeof error === 'object' ? error : {message: String(error)});
  } finally {
    if (entry && command !== 'conversation_start') release(id, entry);
  }
}
export function applyOrbLevel(level) {
  const stage = document.querySelector('.voice-stage');
  if (!stage) return;
  const normalized = Math.max(0, Math.min(1, Number(level) * 4));
  stage.style.setProperty('--orb-scale', String(1 + normalized * .22));
  stage.style.setProperty('--orb-glow', String(.18 + normalized * .5));
}
export function formatDate(value, timeZone, allDay, exclusiveEnd) {
  try {
    const date = new Date(allDay ? `${value}T00:00:00Z` : value);
    if (allDay && exclusiveEnd) date.setUTCDate(date.getUTCDate() - 1);
    return new Intl.DateTimeFormat('pt-BR', allDay
      ? {dateStyle: 'long', timeZone: 'UTC'}
      : {dateStyle: 'long', timeStyle: 'short', timeZone}).format(date);
  } catch { return value; }
}
