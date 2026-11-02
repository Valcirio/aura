import test from 'node:test';
import assert from 'node:assert/strict';
import {request,requestStream,disposeStream,formatDate} from '../public/bridge.js';

test('ponte usa apenas o dispatcher e preserva argumentos Unicode', async () => {
  globalThis.window={__TAURI__:{core:{invoke:async(name,args)=>{
    assert.equal(name,'app_command');assert.equal(args.command,'memory_search');assert.equal(args.args.query,'conexão');
    assert.equal(args.onEvent,undefined);return {ok:true};
  }}}};
  assert.deepEqual(JSON.parse(await request('memory_search',JSON.stringify({query:'conexão'}))),{ok:true});
});
function deferred(){let resolve,reject;const promise=new Promise((a,b)=>{resolve=a;reject=b;});return {promise,resolve,reject};}
class Channel {}
function setup(){
  const calls=[];const d=deferred();
  globalThis.window={__TAURI__:{core:{Channel,invoke:(name,args)=>{calls.push(args);return d.promise;}}}};
  return {calls,d,channel:()=>calls[0].onEvent};
}
const ev=(kind,generation=4,session_id=1)=>({kind,generation,session_id,turn_id:'t',payload:{}});
const START='{"session_id":1}';
let next=100;
const id=()=>++next;

test('channel entrega eventos tipados antes da resposta de início', async () => {
  const s=setup();const events=[];const n=id();
  const p=requestStream('conversation_start',START,e=>events.push(JSON.parse(e)),n);
  s.channel().onmessage(ev('run.started'));
  assert.equal(events.length,0);
  s.d.resolve({session_id:1,turn_id:'a',generation:4});
  const ticket=JSON.parse(await p);
  assert.equal(events.length,1);assert.equal(events[0].generation,ticket.generation);
  disposeStream(n);
});
test('eventos antecipados são reproduzidos em ordem e uma só vez; descasados são ignorados', async () => {
  const s=setup();const kinds=[];const n=id();
  const p=requestStream('conversation_start',START,e=>kinds.push(JSON.parse(e).kind),n);
  const ch=s.channel();
  ch.onmessage(ev('run.started'));ch.onmessage(ev('x',5));ch.onmessage(ev('y',4,2));
  ch.onmessage({...ev('turn.next'),turn_id:'outro'});ch.onmessage(ev('assistant.delta'));
  s.d.resolve({session_id:1,turn_id:'a',generation:4});await p;
  ch.onmessage(ev('state.changed'));ch.onmessage(ev('z',9));
  assert.deepEqual(kinds,['run.started','turn.next','assistant.delta','state.changed']);
  disposeStream(n);
});
test('terminal antes do ticket é reproduzido, libera o canal e ignora eventos tardios', async () => {
  const s=setup();const kinds=[];const n=id();
  const p=requestStream('conversation_start',START,e=>kinds.push(JSON.parse(e).kind),n);
  const ch=s.channel();
  ch.onmessage(ev('run.started'));ch.onmessage(ev('run.completed'));ch.onmessage(ev('late'));
  s.d.resolve({session_id:1,turn_id:'a',generation:4});await p;
  ch.onmessage(ev('late2'));
  assert.deepEqual(kinds,['run.started','run.completed']);
  const again=setup();const q=requestStream('conversation_start',START,()=>{},n);
  again.d.resolve({session_id:1,generation:2});await q;disposeStream(n);
});
test('terminal ao vivo libera o canal e o id pode ser reutilizado', async () => {
  const s=setup();const kinds=[];const n=id();
  const p=requestStream('conversation_start',START,e=>kinds.push(JSON.parse(e).kind),n);
  s.d.resolve({session_id:1,generation:4});await p;
  const ch=s.channel();
  ch.onmessage(ev('turn.cancelled'));ch.onmessage(ev('late'));
  assert.deepEqual(kinds,['turn.cancelled']);
  const again=setup();const q=requestStream('conversation_start',START,()=>{},n);
  again.d.resolve({session_id:1,generation:2});await q;disposeStream(n);
});
test('ticket inválido ou descasado rejeita e libera o canal', async () => {
  for(const ticket of [null,{session_id:2,generation:4},{session_id:1,generation:0},{session_id:1,generation:'4'},{generation:4}]){
    const s=setup();const events=[];const n=id();
    const p=requestStream('conversation_start',START,e=>events.push(e),n);
    s.channel().onmessage(ev('run.started'));
    s.d.resolve(ticket);
    await assert.rejects(p,e=>typeof e==='string'&&JSON.parse(e).code==='INVALID_TICKET');
    assert.equal(events.length,0);
    const again=setup();const q=requestStream('conversation_start',START,()=>{},n);
    again.d.resolve({session_id:1,generation:2});await q;disposeStream(n);
  }
});
test('descarte antes da resolução impede reprodução e reativação', async () => {
  const s=setup();const events=[];const n=id();
  const p=requestStream('conversation_start',START,e=>events.push(e),n);
  const ch=s.channel();const old=ch.onmessage;
  ch.onmessage(ev('run.started'));
  disposeStream(n);
  s.d.resolve({session_id:1,generation:4});await p;
  old(ev('late'));ch.onmessage(ev('late'));
  assert.equal(events.length,0);
  const again=setup();const q=requestStream('conversation_start',START,()=>{},n);
  again.d.resolve({session_id:1,generation:2});await q;disposeStream(n);
});
test('descarte depois da resolução é idempotente e handler antigo não chama callback', async () => {
  const s=setup();const events=[];const n=id();
  const p=requestStream('conversation_start',START,e=>events.push(e),n);
  const old=s.channel().onmessage;
  s.d.resolve({session_id:1,generation:4});await p;
  disposeStream(n);disposeStream(n);disposeStream(99999);
  old(ev('late'));s.channel().onmessage(ev('late'));
  assert.equal(events.length,0);
});
test('falha do invoke rejeita e libera o canal', async () => {
  const s=setup();const n=id();
  const p=requestStream('conversation_start',START,()=>{},n);
  const old=s.channel().onmessage;
  s.d.reject({code:'X',message:'falhou'});
  await assert.rejects(p,e=>JSON.parse(e).code==='X');
  old(ev('late'));
  const again=setup();const q=requestStream('conversation_start',START,()=>{},n);
  again.d.resolve({session_id:1,generation:2});await q;disposeStream(n);
});
test('exceção no callback durante a reprodução rejeita e libera o canal', async () => {
  const s=setup();const n=id();let count=0;
  const p=requestStream('conversation_start',START,()=>{count++;throw new Error('boom');},n);
  const ch=s.channel();
  ch.onmessage(ev('run.started'));ch.onmessage(ev('assistant.delta'));
  s.d.resolve({session_id:1,generation:4});
  await assert.rejects(p,e=>typeof e==='string');
  assert.equal(count,1);
  ch.onmessage(ev('late'));assert.equal(count,1);
  const again=setup();const q=requestStream('conversation_start',START,()=>{},n);
  again.d.resolve({session_id:1,generation:2});await q;disposeStream(n);
});
test('exceção no callback ao vivo libera o canal sem escapar', async () => {
  const s=setup();const n=id();let count=0;
  const p=requestStream('conversation_start',START,()=>{count++;throw new Error('boom');},n);
  s.d.resolve({session_id:1,generation:4});await p;
  s.channel().onmessage(ev('state.changed'));s.channel().onmessage(ev('state.changed'));
  assert.equal(count,1);
});
test('argumentos inválidos rejeitam e não retêm o id', async () => {
  const s=setup();const n=id();
  await assert.rejects(requestStream('conversation_start','{',()=>{},n));
  const q=requestStream('conversation_start',START,()=>{},n);
  s.d.resolve({session_id:1,generation:2});await q;disposeStream(n);
});
test('id duplicado é rejeitado sem substituir o canal ativo', async () => {
  const s=setup();const events=[];const n=id();
  const p=requestStream('conversation_start',START,e=>events.push(e),n);
  await assert.rejects(requestStream('conversation_start',START,()=>{},n),e=>JSON.parse(e).code==='STREAM_IN_USE');
  s.d.resolve({session_id:1,generation:4});await p;
  s.channel().onmessage(ev('state.changed'));
  assert.equal(events.length,1);disposeStream(n);
});
test('progresso de modelo é entregue imediatamente e o canal é liberado ao concluir', async () => {
  const s=setup();const events=[];const n=id();
  const p=requestStream('model_install','{"id":"m"}',e=>events.push(JSON.parse(e)),n);
  const ch=s.channel();
  ch.onmessage({kind:'model.progress',generation:0,session_id:0,payload:{}});
  ch.onmessage({kind:'run.completed',generation:0,session_id:0,payload:{}});
  ch.onmessage({kind:'model.progress',generation:0,session_id:0,payload:{}});
  assert.equal(events.length,3);
  s.d.resolve({ok:true});await p;
  ch.onmessage({kind:'model.progress',generation:0,session_id:0,payload:{}});
  assert.equal(events.length,3);
  const again=setup();const q=requestStream('model_install','{}',()=>{},n);
  again.d.resolve({});await q;
});
test('falha da instalação de modelo libera o canal', async () => {
  const s=setup();const n=id();
  const p=requestStream('model_install','{}',()=>{},n);
  s.d.reject({message:'falhou'});
  await assert.rejects(p);
  const again=setup();const q=requestStream('model_install','{}',()=>{},n);
  again.d.resolve({});await q;
});
test('erros do backend preservam mensagem sem mascarar a falha', async () => {
  globalThis.window={__TAURI__:{core:{invoke:async()=>{throw {code:'AUTH_REQUIRED',message:'Reconecte a conta'};}}}};
  await assert.rejects(request('calendar_list','{}'),error=>JSON.parse(error).code==='AUTH_REQUIRED');
});
test('datas de confirmação usam o fuso e mostram último dia inclusivo', () => {
  assert.match(formatDate('2026-10-30T17:00:00Z','America/Bahia',false,false),/14:00/);
  assert.match(formatDate('2026-10-31','America/Bahia',true,true),/30 de outubro/);
});
