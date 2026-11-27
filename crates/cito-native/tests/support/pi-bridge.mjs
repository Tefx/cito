// purpose: verify the real Pi extension with the structural public host API and no npm packages.
// usage: node --test pi-bridge.mjs (also invoked by Rust cleanroom tests).
// effects: temporary Unix sockets and loopback HTTP only; all resources close in finally.
// requires: Node.js with type stripping and python3.
import assert from 'node:assert/strict';
import fs from 'node:fs';
import test from 'node:test';
import {syncBuiltinESMExports} from 'node:module';
import {createConnection} from 'node:net';
import {createServer} from 'node:http';
import {appendFileSync, mkdirSync, writeFileSync, existsSync, statSync, unlinkSync} from 'node:fs';
import {execFileSync} from 'node:child_process';
import {join} from 'node:path';
import bridge from '../../../../plugins/pi/cito-bridge.ts';
test('real Pi host, socket repair, and shutdown', async t => {
const root=fs.mkdtempSync(join(process.env.TMPDIR || '/tmp','ct-pi-'));
t.after(()=>fs.rmSync(root,{recursive:true,force:true}));
const state=join(root,'state'), path=join(state,'sockets',`pi-pid-${process.pid}.sock`);
const sentinel=join(state,'pi','endpoints',`pi-pid-${process.pid}.watch`);
process.env.HOME=root;
process.env.CITO_STATE_DIR=state;
process.env.CITO_PI_HOME=join(root,'.pi','agent');
process.env.CITO_CONFIG_PATH=join(state,'config.json');
process.env.CITO_PI_ALLOW_HEADLESS_DISCOVERY='1';
const handlers=new Map(), messages=[], badges=[], notices=[], registrations=[], calls=[];
let session='session-a', renderer, toolRenderer, command, attempts=0, failSend=false, stallHttp=false, holdHttp=false, heldArrival;
const held=[];
const branch=[{type:'message',message:{role:'assistant',content:'Not the user prompt'}},
 {type:'message',message:{role:'user',content:[{type:'image'},{type:'text',text:'  Review\u0007\n routing '},{type:'text',text:'🦀'.repeat(140)}]}}];
const about='Review routing '+'🦀'.repeat(105);
const entries=new Map(), steering=[];
let streaming=false, nextEntry=0;
const transcript=()=>join(root,`${session}.jsonl`);
const context={cwd:'/repo',isIdle:()=>!streaming,sessionManager:{getSessionId:()=>session,getSessionName:()=> 'Work',getSessionFile:transcript,getBranch:()=>branch,getEntries:()=>[...branch,...entries.get(session)??[]]},ui:{setStatus:(k,v)=>{assert.equal(k,'cito');badges.push(v);},notify:(s,t)=>notices.push({s,t})}};
function append(message, write=true) {
 const entry={type:'custom_message',id:`entry-${++nextEntry}`,...message};
 if(!entries.has(session))entries.set(session,[]);
 entries.get(session).push(entry);
 if(write) appendFileSync(transcript(),JSON.stringify(entry)+'\n');
 return entry;
}
const api={on:(n,f)=>handlers.set(n,f),registerMcpServer:(name,config)=>{if(++attempts===1)throw Error('Registry not ready');registrations.push({name,config});},registerMessageRenderer:(_n,r)=>{renderer=r},registerToolRenderer:r=>{toolRenderer=r},registerCommand:(_n,c)=>{command=c},sendMessage:(m,o)=>{if(failSend)throw Error('Retry');messages.push({m,o});if(streaming)steering.push(m);else append(m);}};
function request(value,fragment=false){return new Promise((resolve,reject)=>{const s=createConnection(path);let data='';s.setTimeout(2000,()=>s.destroy(Error('Timeout')));s.on('error',reject);s.on('close',()=>reject(Error('Socket closed before receipt')));s.on('connect',()=>{const raw=JSON.stringify(value)+'\n';if(fragment){s.write(raw.slice(0,5));s.end(raw.slice(5));}else s.write(raw);});s.on('data',c=>{data+=c;if(data.includes('\n')){s.destroy();resolve(JSON.parse(data));}});});}
async function waitForSocket() {
 const deadline=Date.now()+2000;
 for(;;) {
  try {assert.deepEqual(await request({op:'ping'}),{ok:true});return;}
  catch(error) {
   if(!['ENOENT','ECONNREFUSED'].includes(error.code)||Date.now()>=deadline)throw error;
   await new Promise(r=>setTimeout(r,10));
  }
 }
}
const server=createServer(async(req,res)=>{let body='';for await(const chunk of req)body+=chunk;calls.push(JSON.parse(body));if(holdHttp){held.push(res);heldArrival?.();return;}if(stallHttp)return;res.end(JSON.stringify({jsonrpc:'2.0',id:1,result:{content:[{type:'text',text:JSON.stringify({ok:true,listener_id:'  alice-very-long-project-abcdefgh1234  ',scope:'self'})}]}}));});
const originalWatch=fs.watch;
let muteDirectory=false;
const watchPaths=new Set();
fs.watch=(file,options,callback)=>{
 watchPaths.add(file);
 assert.equal(options.persistent,false);
 return originalWatch(file,options,(...args)=>{
  if(file!==join(state,'sockets')||!muteDirectory)callback(...args);
 });
};
syncBuiltinESMExports();
t.after(()=>{fs.watch=originalWatch;syncBuiltinESMExports();});
try {
 mkdirSync(join(state,'sockets'),{recursive:true});execFileSync('python3',['-c','import socket,sys; s=socket.socket(socket.AF_UNIX); s.bind(sys.argv[1]); s.close()',path]);
 bridge(api);
 // Startup must recover an F58 stale socket, even before session_start.
 await waitForSocket();
 const sessionDir=join(process.env.HOME,'.pi','agent','sessions',`--${process.cwd().replace(/^\//,'').replaceAll('/','-')}--`);mkdirSync(sessionDir,{recursive:true});
 writeFileSync(join(sessionDir,'latest.jsonl'),JSON.stringify({type:'session',id:'pre-start',name:'Before start'})+'\n{"partial":');
 for(const op of ['identify','status','deliver']) assert.deepEqual(await request({op}),{ok:false,error:'SESSION_NOT_READY'});
 session='baadf00d-cafe-47b1-beca-238e0bd64e9d';await handlers.get('session_start')({},context);
 const recovered=await request({op:'identify'});assert.equal(recovered.session_id,session);
 assert.equal(recovered.session_name,'Work');assert.equal(recovered.about,about);
 // The first session_start owns the transition; all later socket operations use that session.
 const early={op:'deliver',session_id:recovered.session_id,batch_id:'before-start',content:'Wake after session_start',wake:true};
 assert.equal((await request({op:'status',session_id:recovered.session_id,badge:'listening before start'})).ok,true);
 assert.equal(badges.at(-1),'📡');
 const badgeWrites=badges.length;
 session=undefined;
 assert.deepEqual(await request({op:'ping'}),{ok:true});
 const resynced=await request({op:'identify'});
 assert.equal(resynced.session_id,recovered.session_id);
 assert.equal(resynced.badge,'listening before start');
 assert.equal(badges.length,badgeWrites,'Heartbeat and identify must not rewrite an unchanged badge');
 session=recovered.session_id;
 assert.equal((await request({...early,session_id:'wrong'})).error,'SESSION_MISMATCH');
 assert.equal((await request({op:'status',session_id:'wrong',badge:'wrong'})).error,'SESSION_MISMATCH');
 assert.equal((await request(early)).deduplicated,false);assert.equal((await request(early)).deduplicated,true);
 assert.deepEqual(messages,[{m:{customType:'cito-notification',content:early.content,display:true,details:{citoBatchId:early.batch_id}},o:{triggerTurn:true,deliverAs:'steer'}}]);
 session=recovered.session_id;
 await handlers.get('session_start')({},context);await handlers.get('session_start')({},context);
 assert.equal(badges.at(-1),'📡','Repeated session_start must retain the session badge');
 assert.equal((await request(early)).deduplicated,true);assert.equal(messages.length,1,'Repeated session_start must retain batch deduplication');
 session='session-a';await handlers.get('session_tree')({},context);assert.equal(badges.at(-1),undefined);
 messages.length=0;
 assert.equal(attempts,2);assert.deepEqual(registrations,[{name:'cito',config:{command:join(state,'bin/cito-mcp'),args:[]}}]);assert.equal(statSync(path).mode&0o777,0o600);assert.equal(statSync(join(state,'sockets')).mode&0o777,0o700);
 assert.deepEqual(await request({op:'ping'}),{ok:true});assert.equal((await request({op:'identify'})).session_id,'session-a');
 async function checkBadge(badge, ...writes) {
  const before=badges.length;
  assert.deepEqual(await request({op:'status',session_id:session,badge}),{ok:true});
  assert.equal((await request({op:'identify'})).badge,badge);
  assert.deepEqual(badges.slice(before),writes);
 }
 await checkBadge('Cito · alice','📡');
 await checkBadge('Cito · alice (0)');
 await checkBadge('Cito · alice (2)','📡 2');
 await checkBadge('Cito · bob (02)  ');
 await checkBadge('Cito · alice (99)','📡 99');
 await checkBadge('Cito · alice (120)','📡 99+');
 await checkBadge('Cito · alice (121)');
 await checkBadge('',undefined);
 await checkBadge(null);
 context.ui.theme={fg:(c,t)=>`[${c}:${t}]`};
 await checkBadge('Cito · alice','[dim:📡]');
 await checkBadge('Cito · alice (2)','[accent:📡 2]');
 const beforeContextChange=badges.length;
 await handlers.get('session_info_changed')({}, {...context});
 assert.deepEqual(badges.slice(beforeContextChange),['[accent:📡 2]']);
 delete context.ui.theme;
 await handlers.get('session_info_changed')({},context);
 assert.equal(badges.at(-1),'📡 2');
 await checkBadge(null,undefined);
 const delivery={op:'deliver',id:17,session_id:session,batch_id:'one',content:'<card> café',wake:false};assert.equal((await request(delivery,true)).deduplicated,false);assert.equal((await request(delivery)).deduplicated,true);assert.equal(messages.length,1);assert.deepEqual(messages[0],{m:{customType:'cito-notification',content:'<card> café',display:true,details:{citoBatchId:'one'}},o:{triggerTurn:false,deliverAs:'followUp'}});
 assert.equal((await request({...delivery,session_id:'wrong'})).error,'SESSION_MISMATCH');assert.equal((await request({...delivery,batch_id:'invalid',wake:'yes'})).error,'INVALID_REQUEST');
 failSend=true;assert.equal((await request({...delivery,batch_id:'retry'})).error,'BRIDGE_ERROR');failSend=false;assert.equal((await request({...delivery,batch_id:'retry',wake:true})).deduplicated,false);assert.equal(messages.at(-1).o.triggerTurn,true);
 await request({op:'status',session_id:session,badge:'active'});session='session-b';assert.equal((await request({op:'identify'})).session_id,'session-b');assert.equal(badges.at(-1),undefined);
 assert.equal((await request({...delivery,session_id:session})).deduplicated,false);session='session-a';await handlers.get('session_tree')({},context);assert.equal((await request(delivery)).deduplicated,true);
 // A queued wake is not a receipt. Clearing the queue and settling must let
 // the daemon submit the same batch again, even if a new tool is already running.
 const wake={...delivery,batch_id:'during-tool',wake:true};
 const unconfirmed={id:17,ok:true,delivered:false,delivered_unconfirmed:true};
 streaming=true;
 assert.deepEqual(await request(wake),unconfirmed);
 assert.equal(messages.at(-1).o.deliverAs,'steer');
 assert.deepEqual(await request(wake),unconfirmed);
 assert.equal(steering.length,1,'Retries must not queue duplicate steering messages');
 steering.length=0;
 await handlers.get('agent_settled')({},context);
 assert.deepEqual(await request(wake),unconfirmed);
 assert.equal(steering.length,1,'A dropped steer must be eligible for reinjection');
 const memoryOnly=append(steering.shift(),false);
 assert.deepEqual(await request(wake),unconfirmed,'A session-manager entry alone is not a durable receipt');
 appendFileSync(transcript(),JSON.stringify(memoryOnly)+'\n');
 assert.deepEqual(await request(wake),{id:17,ok:true,delivered:true,persisted_batch_id:wake.batch_id,deduplicated:true});
 assert.equal(steering.length,0);
 streaming=false;
 await handlers.get('agent_settled')({},context);
 const plain={fg:(_n,t)=>t,bg:(_n,t)=>t,bold:t=>t};
 const painted={...plain,bg:(color,text)=>`<${color}>${text}</${color}>`};
 const padding='<customMessageBg>'+' '.repeat(40)+'</customMessageBg>';
 const send=toolRenderer('mcp__cito__cito_send',()=>undefined);
 const args={to:'bob/agent#123'};
 const renderContext={isError:false,outputPad:1,state:{}};
 const title=send.renderCall(args,painted,renderContext);
 for(const card of [renderer({content:'Cito · hello'},{expanded:false},painted),title]) {
  const lines=card.render(40);assert.equal(lines[0],padding);assert.equal(lines.at(-1),padding);assert.match(lines[1],/Cito ·/);
 }
 const receipt={content:[{type:'text',text:'{"signal_id":"sig_1","expires_at":"later"}'}]};
 for(const expanded of [true,false]) {
  const rendered=send.renderResult(receipt,{expanded},plain,renderContext).render(60).join('\n');
  assert.match(rendered,/sig_1/);assert.equal(rendered.includes('expires_at'),expanded);
 }
 assert.deepEqual(title.render(60),[],'The call frame must disappear once the combined result card is available');
 function renderTool(action, receipt) {
  const tool=toolRenderer(`mcp__cito__cito_${action}`,()=>undefined);
  return tool.renderResult({content:JSON.stringify(receipt)},{expanded:false},plain,{...renderContext,state:{}}).render(120).join('\n');
 }
 assert.match(renderTool('send',{ok:true,signal_id:'sig_1',maintenance_warning:'Sent history unavailable'}),/sig_1[\s\S]*Warning: Sent history unavailable/);
 const found=renderTool('find',{matched_count:2,online_count:1,listeners:[{display_address:'alice/agent#review',listener_id:'opaque'},{display_address:'bob/agent#test'}]});
 assert.match(found,/2 listeners · 1 online[\s\S]*alice\/agent#review[\s\S]*bob\/agent#test/);
 assert.doesNotMatch(found,/opaque|display_address/);
 const status=renderTool('status',{registered:true,unread_count:2,display_address:'alice/agent#review',hubs:[{name:'work',status:'auth_required'},{name:'cloud',status:'disconnected'}]});
 assert.match(status,/Listening · 2 unread[\s\S]*Warning: work: Sign in required[\s\S]*Warning: cloud: Disconnected/);
 assert.equal(toolRenderer('mcp__other__cito_send',()=> 'fallback'),'fallback');
 const narrow=renderer({content:'\u4e2d👩‍💻é'.repeat(8)},{expanded:false,outputPad:4},plain);
 for(let width=1;width<=12;width++)for(const line of narrow.render(width)){const cells=Array.from(line.replaceAll('\u4e2d','XX').replaceAll('👩‍💻','XX').replaceAll('é','e')).length;assert(cells<=width,`width ${width}: ${line}`);}
 let c=0;const theme={fg:(_n,t)=>{c++;return t},bg:(_n,t)=>t,bold:t=>t};const card=renderer({content:[{type:'text',text:'hello café 👩‍💻 é'}]},{expanded:false},theme);assert(card.render(12).some(s=>s.includes('hello')));assert.deepEqual(card.render(0),[]);const before=c;card.invalidate();card.render(10);assert(c>before);
 await new Promise(r=>server.listen(0,'127.0.0.1',r));const port=server.address().port;writeFileSync(join(state,'config.json'),JSON.stringify({daemonPort:1}));process.env.CITO_DAEMON_PORT=' '+port+' ';
 for(const arg of ['on --hub Work-Internal Review','team --hub=Work-Internal Pair','off --hub Work-Internal']) {
  await command.handler(arg,context);
  if(arg.startsWith('on')) {
   assert.equal(badges.at(-1),'📡');
   assert.equal((await request({op:'identify'})).badge,'Cito · alice-very-l');
  }
 }
 assert.deepEqual(calls.map(v=>v.params.arguments),[{about:'Review',scope:'self',hub:'Work-Internal'},{about:'Pair',scope:'workspace',hub:'Work-Internal'},{hub:'Work-Internal'}]);assert(calls.every(v=>v.params._meta.endpoint_pid===process.pid));assert(calls.every(v=>v.params._meta.expected_session_id==='session-a'));
 process.env.CITO_DAEMON_PORT='0';process.env.CITO_CONFIG_PATH='~/bridge-config.json';writeFileSync(join(process.env.HOME,'bridge-config.json'),JSON.stringify({daemonPort:port}));await command.handler('on',context);assert.equal(calls.length,4);assert.equal(calls.at(-1).params.arguments.about,about);
 await command.handler('on --hub INVALID!',context);assert.equal(calls.length,4);assert.equal(notices.at(-1).t,'warning');
 assert.deepEqual(command.getArgumentCompletions('t'),[{value:'team',label:'team'}]);
 // The daemon sees each mutation only after the prior same-session request settles.
 holdHttp=true;
 const begin=async arg=>{const arrived=new Promise(resolve=>{heldArrival=resolve});const done=command.handler(arg,context);await arrived;return {done,response:held.at(-1)}};
 for(const staleError of [false,true]) {
  const older=await begin('private Older');const count=calls.length;
  const newer=begin('off');
  // A bridge round trip gives a mistakenly concurrent 'off' time to arrive.
  await request({op:'ping'});
  assert.equal(calls.length,count,'The unlisten must not reach the daemon before listen settles');
  const reply=JSON.stringify({jsonrpc:'2.0',id:1,result:{content:[{type:'text',text:JSON.stringify({ok:true,listener_id:'older-session',scope:'self'})}]}});
  older.response.statusCode=staleError?500:200;older.response.end(staleError?'Unavailable':reply);await older.done;
  const off=await newer;assert.equal(calls.at(-1).params.name,'cito_unlisten');off.response.end(reply);await off.done;
  assert.equal(badges.at(-1),undefined);
  assert.equal(notices.at(-1).s,'Cito stopped listening');
 }
 // A blocked session does not block another session or resurrect its queued
 // intent when the user switches away and returns to the original ID.
 {
  const start=calls.length;
  const older=await begin('team Pending');const queued=command.handler('off',context);
  session='session-b';await handlers.get('session_tree')({},context);
  const other=await begin('on Independent');
  assert.deepEqual(calls.slice(start).map(v=>[v.params.name,v.params._meta.expected_session_id]),[['cito_listen','session-a'],['cito_listen','session-b']]);
  const reply=JSON.stringify({result:{content:[{type:'text',text:JSON.stringify({ok:true,listener_id:'active',scope:'self'})}]}});
  other.response.end(reply);await other.done;
  session='session-a';await handlers.get('session_tree')({},context);
  const badgeCount=badges.length, noticeCount=notices.length;
  older.response.end(reply);await older.done;await queued;
  assert.equal(calls.length,start+2);assert.equal(badges.length,badgeCount);assert.equal(notices.length,noticeCount);
 }
 // Both transitions retain the previous badge when the daemon sends no usable receipt.
 for(const [op,badge] of [['on',undefined],['off','active']]) {
  await request({op:'status',session_id:session,badge});
  for(const content of [undefined,[],[{type:'text',text:''}],...['{}','[]','null','1','not JSON'].map(text=>[{type:'text',text}])]) {
   const before=badges.length;
   const {response,done}=await begin(op);
   response.end(JSON.stringify({jsonrpc:'2.0',id:1,result:{content}}));await done;
   assert.equal(notices.at(-1).t,'error');assert.match(notices.at(-1).s,/Invalid cito_(un)?listen receipt/);
   assert.equal(badges.length,before);assert.equal((await request({op:'identify'})).badge,badge??null);
  }
 }
 holdHttp=false;
 const configPath=join(process.env.HOME,'bridge-config.json'), beforeMalformed=calls.length;
 writeFileSync(configPath,'{invalid');await command.handler('on',context);
 assert.equal(calls.length,beforeMalformed,'Malformed config must not fall back to another daemon');
 assert.equal(notices.at(-1).t,'error');assert.match(notices.at(-1).s,/Invalid JSON/);
 writeFileSync(configPath,JSON.stringify({daemonPort:port}));
 // Exercise actual fetch cancellation without a 15-second wall-clock test.
 const timeout=AbortSignal.timeout;let timeoutRequested;
 AbortSignal.timeout=milliseconds=>{timeoutRequested=milliseconds;return timeout(20);};
 stallHttp=true;
 try {await command.handler('on',context);assert.equal(timeoutRequested,15000);assert.equal(notices.at(-1).t,'error');}
 finally {AbortSignal.timeout=timeout;stallHttp=false;}
 // A fresh module must identify the same session before the next session_start.
 session='edd50a9a-982b-447a-8541-bbb3b2c8531a';
 await handlers.get('session_start')({},context);
 const beforeReload={...early,session_id:session,batch_id:'reload'};
 assert.equal((await request(beforeReload)).deduplicated,false);
 await request({op:'status',session_id:session,badge:'Retained badge'});
 holdHttp=true;
 const acrossReload=await begin('on Pending reload');
 const reloadCallCount=calls.length;
 await handlers.get('session_shutdown')({reason:'reload'},context);
 const reloaded=await import('../../../../plugins/pi/cito-bridge.ts?reload');
 reloaded.default(api);
 await waitForSocket();
 const current=await request({op:'identify'});
 assert.deepEqual([current.session_id,current.cwd,current.session_name,current.about,current.badge],[session,'/repo','Work',about,'Retained badge']);
 await handlers.get('session_start')({reason:'reload'},context);
 assert.equal((await request(beforeReload)).deduplicated,true);
 assert.equal(badges.at(-1),'📡');
 const offAfterReload=begin('');
 await request({op:'ping'});
 assert.equal(calls.length,reloadCallCount,'The new command must wait for the old instance’s in-flight request');
 const reloadReply=JSON.stringify({result:{content:[{type:'text',text:JSON.stringify({ok:true,listener_id:'active',scope:'self'})}]}});
 acrossReload.response.end(reloadReply);await acrossReload.done;
 const off=await offAfterReload;
 assert.equal(calls.at(-1).params.name,'cito_unlisten');
 off.response.end(reloadReply);await off.done;
 assert.equal(badges.at(-1),undefined);
 holdHttp=false;
 // U2: recover an unlinked socket only while the package remains configured.
 const home=join(process.env.HOME,'.pi','agent');mkdirSync(home,{recursive:true});
 const custom=join(process.env.HOME,'custom-pi');mkdirSync(custom,{recursive:true});
 writeFileSync(join(custom,'cito-bridge.ts'),'');
 writeFileSync(join(home,'settings.json'),JSON.stringify({packages:['~/custom-pi']}));
 assert(watchPaths.has(join(state,'sockets')));
 assert(watchPaths.has(sentinel));
 assert(statSync(sentinel).isFile());
 assert.equal(statSync(sentinel).mode&0o777,0o600);
 // Suppress directory notifications. Only the real regular-file fs.watch can
 // repair the socket now; neither an extension lifecycle event nor a timer can.
 muteDirectory=true;
 const interval=globalThis.setInterval, watchFile=fs.watchFile;
 globalThis.setInterval=fs.watchFile=()=>{assert.fail('Socket repair must not poll');};
 syncBuiltinESMExports();
 try {
  for(let i=0;i<2;i++) {
   unlinkSync(path);
   writeFileSync(sentinel,'1',{flag:'r+'});
   await waitForSocket();
   assert.equal((await request({op:'identify'})).session_id,session);
  }
 } finally {globalThis.setInterval=interval;fs.watchFile=watchFile;syncBuiltinESMExports();muteDirectory=false;}
 // Remaining cases exercise repair policy. A real session_start is a repair
 // opportunity even if FSEvents batched two successive unlinks into one event.
 async function unlinkAndResume() {
  unlinkSync(path);
  await handlers.get('session_start')({},context);
 }
 for(const entry of [join(state,'runtime','plugins','pi'),join(state,'plugins','pi'),'~/custom-pi']) {
  writeFileSync(join(home,'settings.json'),JSON.stringify({packages:[entry]}));
  await unlinkAndResume();
  for(let i=0;i<100&&!existsSync(path);i++)await new Promise(r=>setTimeout(r,10));
  assert(existsSync(path),`Missing repaired socket for ${entry}`);
  assert.equal((await request({op:'identify'})).session_id,session,entry);
 }
 // Delete the replacement itself while repair is finishing its permission setup.
 const chmod=fs.chmodSync;let interrupted=0;
 fs.chmodSync=(file,mode)=>{chmod(file,mode);if(file===path && interrupted++===0)unlinkSync(path);};
 syncBuiltinESMExports();
 try {
  await unlinkAndResume();
  for(let i=0;i<100 && interrupted<2;i++)await new Promise(r=>setTimeout(r,10));
  assert(interrupted>=2,'A second unlink during repair must start another bind');
  assert.equal((await request({op:'identify'})).session_id,session);
 } finally {fs.chmodSync=chmod;syncBuiltinESMExports();}
 for(const packages of [[],[join(process.env.HOME,'unrelated-package')]]) {
  writeFileSync(join(home,'settings.json'),JSON.stringify({packages}));
  if(existsSync(path))await unlinkAndResume();
  await new Promise(r=>setTimeout(r,50));assert.equal(existsSync(path),false);
 }
} finally {await handlers.get('session_shutdown')?.({},context);server.closeAllConnections();if(server.listening)await new Promise(r=>server.close(r));}
assert.equal(existsSync(path),false);
assert.equal(existsSync(sentinel),false);
process.env.CITO_STATE_DIR='';
assert.throws(()=>bridge({...api,registerMcpServer:()=>assert.fail('Empty state directory reached MCP setup')}),/CITO_STATE_DIR must not be empty/);
process.env.CITO_STATE_DIR=state;
const headless=new Map(), savedArgs=[...process.argv], savedHeadless=process.env.CITO_PI_ALLOW_HEADLESS_DISCOVERY;
try {
 process.argv.push('--mode=json');process.env.CITO_PI_ALLOW_HEADLESS_DISCOVERY='0';
 fs.watch=()=>{throw Error('Headless mode must not register a repair watcher');};syncBuiltinESMExports();
 bridge({...api,on:(n,f)=>headless.set(n,f),registerMcpServer:()=>{}});
 await headless.get('session_start')({},context);
 assert.equal(existsSync(path),false);
} finally {
 await headless.get('session_shutdown')?.({},context);fs.watch=originalWatch;syncBuiltinESMExports();process.argv.splice(0,process.argv.length,...savedArgs);
 if(savedHeadless===undefined)delete process.env.CITO_PI_ALLOW_HEADLESS_DISCOVERY;else process.env.CITO_PI_ALLOW_HEADLESS_DISCOVERY=savedHeadless;
}
const broken=new Map();
try {
 bridge({...api,on:(n,f)=>broken.set(n,f),registerMcpServer:()=>{throw Error('Registry failed')}});
 await assert.rejects(()=>broken.get('session_start')({},context),/Registry failed/);
} finally {await broken.get('session_shutdown')?.({},context);}
});
