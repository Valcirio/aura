import test from 'node:test';
import assert from 'node:assert/strict';
import {request,requestStream,formatDate} from '../public/bridge.js';

test('ponte usa apenas o dispatcher e preserva argumentos Unicode', async () => {
  globalThis.window={__TAURI__:{core:{invoke:async(name,args)=>{
    assert.equal(name,'app_command');assert.equal(args.command,'memory_search');assert.equal(args.args.query,'conexão');
    assert.equal(args.onEvent,undefined);return {ok:true};
  }}}};
  assert.deepEqual(JSON.parse(await request('memory_search',JSON.stringify({query:'conexão'}))),{ok:true});
});
test('channel entrega eventos tipados antes da resposta de início', async () => {
  class Channel {}
  globalThis.window={__TAURI__:{core:{Channel,invoke:async(name,args)=>{
    assert.equal(name,'app_stream');assert.ok(args.onEvent instanceof Channel);
    args.onEvent.onmessage({generation:4,kind:'run.started',payload:{}});return {generation:4};
  }}}};
  const events=[];
  const ticket=JSON.parse(await requestStream('conversation_start','{"session_id":1}',event=>events.push(JSON.parse(event))));
  assert.equal(events[0].generation,ticket.generation);
});
test('erros do backend preservam mensagem sem mascarar a falha', async () => {
  globalThis.window={__TAURI__:{core:{invoke:async()=>{throw {code:'AUTH_REQUIRED',message:'Reconecte a conta'};}}}};
  await assert.rejects(request('calendar_list','{}'),error=>JSON.parse(error).code==='AUTH_REQUIRED');
});
test('datas de confirmação usam o fuso e mostram último dia inclusivo', () => {
  assert.match(formatDate('2026-10-30T17:00:00Z','America/Bahia',false,false),/14:00/);
  assert.match(formatDate('2026-10-31','America/Bahia',true,true),/30 de outubro/);
});
