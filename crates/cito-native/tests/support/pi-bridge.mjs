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
import {mkdirSync, writeFileSync, readFileSync, existsSync, statSync, unlinkSync} from 'node:fs';
import {execFileSync} from 'node:child_process';
import {join} from 'node:path';
import bridge from '../../../../plugins/pi/cito-bridge.ts';
test('real Pi host, socket repair, and shutdown', async t => {
const root=fs.mkdtempSync('/tmp/ct-pi-');
t.after(()=>fs.rmSync(root,{recursive:true,force:true}));
const state=join(root,'state'), path=join(state,'sockets',`pi-pid-${process.pid}.sock`);
process.env.HOME=root;
process.env.CITO_STATE_DIR=state;
process.env.CITO_PI_HOME=join(root,'.pi','agent');
process.env.CITO_CONFIG_PATH=join(state,'config.json');
process.env.CITO_PI_ALLOW_HEADLESS_DISCOVERY='1';
const handlers=new Map(), messages=[], badges=[], notices=[], registrations=[], calls=[];
let session='session-a', renderer, command, attempts=0, failSend=false, stallHttp=false, holdHttp=false, heldArrival;
const held=[];
const context={cwd:'/repo',sessionManager:{getSessionId:()=>session,getSessionName:()=> 'Work'},ui:{setStatus:(_k,v)=>badges.push(v),notify:(s,t)=>notices.push({s,t})}};
const api={on:(n,f)=>handlers.set(n,f),registerMcpServer:(name,config)=>{if(++attempts===1)throw Error('Registry not ready');registrations.push({name,config});},registerMessageRenderer:(_n,r)=>{renderer=r},registerCommand:(_n,c)=>{command=c},sendMessage:(m,o)=>{if(failSend)throw Error('Retry');messages.push({m,o});}};
function request(value,fragment=false){return new Promise((resolve,reject)=>{const s=createConnection(path);let data='';s.setTimeout(2000,()=>s.destroy(Error('Timeout')));s.on('error',reject);s.on('connect',()=>{const raw=JSON.stringify(value)+'\n';if(fragment){s.write(raw.slice(0,5));s.end(raw.slice(5));}else s.write(raw);});s.on('data',c=>{data+=c;if(data.includes('\n')){s.destroy();resolve(JSON.parse(data));}});});}
const server=createServer(async(req,res)=>{let body='';for await(const chunk of req)body+=chunk;calls.push(JSON.parse(body));if(holdHttp){held.push(res);heldArrival?.();return;}if(stallHttp)return;res.end(JSON.stringify({jsonrpc:'2.0',id:1,result:{content:[{type:'text',text:JSON.stringify({ok:true,listener_id:'  alice-very-long-project-abcdefgh1234  ',scope:'self'})}]}}));});
try {
 mkdirSync(join(state,'sockets'),{recursive:true});execFileSync('python3',['-c','import socket,sys; s=socket.socket(socket.AF_UNIX); s.bind(sys.argv[1]); s.close()',path]);
 const interval=globalThis.setInterval;
 globalThis.setInterval=(callback,delay,...args)=>{
  assert.notEqual(delay,2000,'A healthy owned socket must not install periodic stat polling');
  return interval(callback,delay,...args);
 };
 try { bridge(api); } finally { globalThis.setInterval=interval; }
 // Startup must recover an F58 stale socket, even before session_start.
 for(let i=0;i<100;i++){try{if((await request({op:'ping'})).ok)break;}catch{}await new Promise(r=>setTimeout(r,10));}
 const fallback=await request({op:'identify'});assert.equal(fallback.ok,true);assert.equal(fallback.session_id,`pid-${process.pid}`);assert.equal(fallback.cwd,process.cwd());
 const sessionDir=join(process.env.HOME,'.pi','agent','sessions',`--${process.cwd().replace(/^\//,'').replaceAll('/','-')}--`);mkdirSync(sessionDir,{recursive:true});
 writeFileSync(join(sessionDir,'latest.jsonl'),JSON.stringify({type:'session',id:'pre-start',name:'Before start'})+'\n{"partial":');
 let recovered, reads=0;
 const originalRead=fs.readFileSync;
 fs.readFileSync=(file,...args)=>{if(file===join(sessionDir,'latest.jsonl'))reads++;return originalRead(file,...args);};
 syncBuiltinESMExports();
 try {
  recovered=await request({op:'identify'});await request({op:'identify'});
  assert.equal(reads,0,'An external session file must never steal the pre-start process identity');
 } finally {fs.readFileSync=originalRead;syncBuiltinESMExports();}
 assert.equal(recovered.ok,true);assert.equal(recovered.session_id,`pid-${process.pid}`);
 session='pre-start';await handlers.get('session_start')({},context);
 recovered=await request({op:'identify'});assert.equal(recovered.session_id,'pre-start');
 // The first session_start owns the transition; all later socket operations use that session.
 const early={op:'deliver',session_id:recovered.session_id,batch_id:'before-start',content:'Wake after session_start',wake:true};
 assert.equal((await request({op:'status',session_id:recovered.session_id,badge:'listening before start'})).ok,true);
 assert.equal(badges.at(-1),'listening before start');
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
 assert.deepEqual(messages,[{m:{customType:'cito-notification',content:early.content,display:true},o:{triggerTurn:true,deliverAs:'followUp'}}]);
 session=recovered.session_id;
 await handlers.get('session_start')({},context);await handlers.get('session_start')({},context);
 assert.equal(badges.at(-1),'listening before start','Repeated session_start must retain the session badge');
 assert.equal((await request(early)).deduplicated,true);assert.equal(messages.length,1,'Repeated session_start must retain batch deduplication');
 session='session-a';await handlers.get('session_tree')({},context);assert.equal(badges.at(-1),undefined);
 messages.length=0;
 assert.equal(attempts,2);assert.deepEqual(registrations,[{name:'cito',config:{command:join(state,'bin/cito-mcp'),args:[]}}]);assert.equal(statSync(path).mode&0o777,0o600);assert.equal(statSync(join(state,'sockets')).mode&0o777,0o700);
 assert.deepEqual(await request({op:'ping'}),{ok:true});assert.equal((await request({op:'identify'})).session_id,'session-a');
 const delivery={op:'deliver',id:17,session_id:session,batch_id:'one',content:'<card> café',wake:false};assert.equal((await request(delivery,true)).deduplicated,false);assert.equal((await request(delivery)).deduplicated,true);assert.equal(messages.length,1);assert.deepEqual(messages[0],{m:{customType:'cito-notification',content:'<card> café',display:true},o:{triggerTurn:false,deliverAs:'followUp'}});
 assert.equal((await request({...delivery,session_id:'wrong'})).error,'SESSION_MISMATCH');assert.equal((await request({...delivery,batch_id:'invalid',wake:'yes'})).error,'INVALID_REQUEST');
 failSend=true;assert.equal((await request({...delivery,batch_id:'retry'})).error,'BRIDGE_ERROR');failSend=false;assert.equal((await request({...delivery,batch_id:'retry',wake:true})).deduplicated,false);assert.equal(messages.at(-1).o.triggerTurn,true);
 await request({op:'status',session_id:session,badge:'active'});session='session-b';assert.equal((await request({op:'identify'})).session_id,'session-b');assert.equal(badges.at(-1),undefined);
 assert.equal((await request({...delivery,session_id:session})).deduplicated,false);session='session-a';await handlers.get('session_tree')({},context);assert.equal((await request(delivery)).deduplicated,true);
 for(let i=0;i<201;i++)await request({...delivery,batch_id:`evict-${i}`});assert.equal((await request(delivery)).deduplicated,false);
 for(let i=0;i<128;i++){session=`inactive-${i}`;await handlers.get('session_tree')({},context);}
 session='session-a';await handlers.get('session_tree')({},context);
 assert.equal((await request(delivery)).deduplicated,false,'The oldest inactive session must be evicted');
 assert.equal((await request(delivery)).deduplicated,true,'The active session must retain deduplication');
 const plain={fg:(_n,t)=>t,bg:(_n,t)=>t,bold:t=>t};
 const narrow=renderer({content:'\u4e2d👩‍💻é'.repeat(8)},{expanded:false,outputPad:4},plain);
 for(let width=1;width<=12;width++)for(const line of narrow.render(width)){const cells=Array.from(line.replaceAll('\u4e2d','XX').replaceAll('👩‍💻','XX').replaceAll('é','e')).length;assert(cells<=width,`width ${width}: ${line}`);}
 let c=0;const theme={fg:(_n,t)=>{c++;return t},bg:(_n,t)=>t,bold:t=>t};const card=renderer({content:[{type:'text',text:'hello café 👩‍💻 é'}]},{expanded:false},theme);assert(card.render(12).some(s=>s.includes('hello')));assert.deepEqual(card.render(0),[]);const before=c;card.invalidate();card.render(10);assert(c>before);
 await new Promise(r=>server.listen(0,'127.0.0.1',r));const port=server.address().port;writeFileSync(join(state,'config.json'),JSON.stringify({daemonPort:1}));process.env.CITO_DAEMON_PORT=' '+port+' ';
 for(const arg of ['on --hub work Review','team --hub=work Pair','off --hub work']) {
  await command.handler(arg,context);
  if(arg.startsWith('on'))assert.equal(badges.at(-1),'Cito · alice-very-l');
 }
 assert.deepEqual(calls.map(v=>v.params.arguments),[{about:'Review',scope:'self',hub:'work'},{about:'Pair',scope:'workspace',hub:'work'},{hub:'work'}]);assert(calls.every(v=>v.params._meta.endpoint_pid===process.pid));assert(calls.every(v=>v.params._meta.expected_session_id==='session-a'));
 process.env.CITO_DAEMON_PORT='0';process.env.CITO_CONFIG_PATH='~/bridge-config.json';writeFileSync(join(process.env.HOME,'bridge-config.json'),JSON.stringify({daemonPort:port}));await command.handler('on',context);assert.equal(calls.length,4);
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
 // U2: recover an unlinked socket only while the package remains configured.
 const home=join(process.env.HOME,'.pi','agent');mkdirSync(home,{recursive:true});
 const custom=join(process.env.HOME,'custom-pi');mkdirSync(custom,{recursive:true});
 writeFileSync(join(custom,'cito-bridge.ts'),readFileSync(new URL('../../../../plugins/pi/cito-bridge.ts',import.meta.url)));
 // Exercise the idle directory watcher without a host callback. Directory events
 // may coalesce; the observable result is a new socket serving the same session.
 writeFileSync(join(home,'settings.json'),JSON.stringify({packages:['~/custom-pi']}));
 unlinkSync(path);
 // On Darwin, a socket-only unlink need not generate its own FSEvents frame.
 // Make distinct sibling directory entries until a real watcher wake is delivered;
 // no callback is injected and only the bridge can recreate the missing socket.
 for(let i=0;i<100 && !existsSync(path);i++) {
  writeFileSync(join(state,'sockets',`sibling-${i}`),'');
  await new Promise(r=>setTimeout(r,100));
 }
 assert.equal((await request({op:'identify'})).session_id,session);
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
 // Run real socket repairs, shortening only the scheduled retry intervals.
 const schedule=globalThis.setTimeout, delays=[];
 let failures=0;
 fs.chmodSync=(file,mode)=>{
  if(file===join(state,'sockets') && failures>0){failures--;throw Error('Directory unavailable');}
  chmod(file,mode);
 };
 syncBuiltinESMExports();
 globalThis.setTimeout=(callback,ms,...args)=>{
  delays.push(ms);return schedule(callback,1,...args);
 };
 try {
  for(const expected of [[50,100,200,400,800,1600,3200,5000,5000],[50]]) {
   failures=expected.length;delays.length=0;
   await unlinkAndResume();
   for(let i=0;i<100&&!existsSync(path);i++)await new Promise(r=>schedule(r,5));
   assert.equal((await request({op:'identify'})).session_id,session);
   assert.deepEqual(delays,expected,'Retry delays must grow, cap, and reset after a healthy bind');
  }
 } finally {globalThis.setTimeout=schedule;fs.chmodSync=chmod;syncBuiltinESMExports();}
 for(const packages of [[],[join(process.env.HOME,'unrelated-package')]]) {
  writeFileSync(join(home,'settings.json'),JSON.stringify({packages}));
  if(existsSync(path))await unlinkAndResume();
  await new Promise(r=>setTimeout(r,50));assert.equal(existsSync(path),false);
 }
} finally {await handlers.get('session_shutdown')?.({},context);server.closeAllConnections();if(server.listening)await new Promise(r=>server.close(r));}
assert.equal(existsSync(path),false);
const headless=new Map(), originalWatch=fs.watch, savedArgs=[...process.argv], savedHeadless=process.env.CITO_PI_ALLOW_HEADLESS_DISCOVERY;
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
