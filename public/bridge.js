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
export async function requestStream(command, argsJson, callback) {
  try {
    const api = core();
    const channel = new api.Channel();
    channel.onmessage = event => callback(JSON.stringify(event));
    return JSON.stringify(await api.invoke('app_stream', {command, args: JSON.parse(argsJson), onEvent: channel}));
  } catch (error) { throw JSON.stringify(typeof error === 'object' ? error : {message: String(error)}); }
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
