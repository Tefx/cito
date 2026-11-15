// purpose: verify the real Pi extension with the structural public host API and no npm packages.
// usage: node --experimental-strip-types pi-bridge.mjs (invoked by Rust cleanroom tests).
// effects: temporary Unix sockets and loopback HTTP only; all resources close in finally.
// requires: Node.js with type stripping and isolated HOME/CITO_STATE_DIR.
import assert from 'node:assert/strict';
import fs from 'node:fs';
import {syncBuiltinESMExports} from 'node:module';
import {createConnection} from 'node:net';
import {createServer} from 'node:http';
import {mkdirSync, writeFileSync, readFileSync, existsSync, statSync, unlinkSync} from 'node:fs';
import {execFileSync} from 'node:child_process';
import {join} from 'node:path';
import bridge from '../../../../plugins/pi/cito-bridge.ts';
const state=process.env.CITO_STATE_DIR, path=join(state,'sockets',`pi-pid-${process.pid}.sock`);
const handlers=new Map(), messages=[], badges=[], notices=[], registrations=[], calls=[];
let session='session-a', renderer, command, attempts=0, failSend=false, stallHttp=false;
const context={cwd:'/repo',sessionManager:{getSessionId:()=>session,getSessionName:()=> 'Work'},ui:{setStatus:(_k,v)=>badges.push(v),notify:(s,t)=>notices.push({s,t})}};
const api={on:(n,f)=>handlers.set(n,f),registerMcpServer:(name,config)=>{if(++attempts===1)throw Error('Registry not ready');registrations.push({name,config});},registerMessageRenderer:(_n,r)=>{renderer=r},registerCommand:(_n,c)=>{command=c},sendMessage:(m,o)=>{if(failSend)throw Error('Retry');messages.push({m,o});}};
function request(value,fragment=false){return new Promise((resolve,reject)=>{const s=createConnection(path);let data='';s.setTimeout(2000,()=>s.destroy(Error('Timeout')));s.on('error',reject);s.on('connect',()=>{const raw=JSON.stringify(value)+'\n';if(fragment){s.write(raw.slice(0,5));s.end(raw.slice(5));}else s.write(raw);});s.on('data',c=>{data+=c;if(data.includes('\n')){s.destroy();resolve(JSON.parse(data));}});});}
const server=createServer(async(req,res)=>{let body='';for await(const chunk of req)body+=chunk;calls.push(JSON.parse(body));if(stallHttp)return;res.end(JSON.stringify({jsonrpc:'2.0',id:1,result:{content:[{type:'text',text:JSON.stringify({ok:true,listener_id:'alice-agent',scope:'self'})}]}}));});
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
 const plain={fg:(_n,t)=>t,bg:(_n,t)=>t,bold:t=>t};
 const narrow=renderer({content:'\u4e2d👩‍💻é'.repeat(8)},{expanded:false,outputPad:4},plain);
 for(let width=1;width<=12;width++)for(const line of narrow.render(width)){const cells=Array.from(line.replaceAll('\u4e2d','XX').replaceAll('👩‍💻','XX').replaceAll('é','e')).length;assert(cells<=width,`width ${width}: ${line}`);}
 let c=0;const theme={fg:(_n,t)=>{c++;return t},bg:(_n,t)=>t,bold:t=>t};const card=renderer({content:[{type:'text',text:'hello café 👩‍💻 é'}]},{expanded:false},theme);assert(card.render(12).some(s=>s.includes('hello')));assert.deepEqual(card.render(0),[]);const before=c;card.invalidate();card.render(10);assert(c>before);
 await new Promise(r=>server.listen(0,'127.0.0.1',r));const port=server.address().port;writeFileSync(join(state,'config.json'),JSON.stringify({daemonPort:1}));process.env.CITO_DAEMON_PORT=' '+port+' ';
 for(const arg of ['on --hub work Review','team --hub=work Pair','off --hub work'])await command.handler(arg,context);
 assert.deepEqual(calls.map(v=>v.params.arguments),[{about:'Review',scope:'self',hub:'work'},{about:'Pair',scope:'workspace',hub:'work'},{hub:'work'}]);assert(calls.every(v=>v.params._meta.endpoint_pid===process.pid));
 process.env.CITO_DAEMON_PORT='0';process.env.CITO_CONFIG_PATH='~/bridge-config.json';writeFileSync(join(process.env.HOME,'bridge-config.json'),JSON.stringify({daemonPort:port}));await command.handler('on',context);assert.equal(calls.length,4);
 await command.handler('on --hub INVALID!',context);assert.equal(calls.length,4);assert.equal(notices.at(-1).t,'warning');
 assert.deepEqual(command.getArgumentCompletions('t'),[{value:'team',label:'team'}]);
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
 for(const entry of [join(state,'runtime','plugins','pi'),join(state,'plugins','pi'),'~/custom-pi']) {
  writeFileSync(join(home,'settings.json'),JSON.stringify({packages:[entry]}));unlinkSync(path);
  for(let i=0;i<100&&!existsSync(path);i++)await new Promise(r=>setTimeout(r,10));
  assert.equal((await request({op:'identify'})).session_id,session,entry);
 }
 for(const packages of [[],[join(process.env.HOME,'unrelated-package')]]) {
  writeFileSync(join(home,'settings.json'),JSON.stringify({packages}));
  if(existsSync(path))unlinkSync(path);
  await new Promise(r=>setTimeout(r,50));assert.equal(existsSync(path),false);
 }
} finally {await handlers.get('session_shutdown')?.({},context);server.closeAllConnections();if(server.listening)await new Promise(r=>server.close(r));}
assert.equal(existsSync(path),false);
const broken=new Map();bridge({...api,on:(n,f)=>broken.set(n,f),registerMcpServer:()=>{throw Error('Registry failed')}});await assert.rejects(()=>broken.get('session_start')({},context),/Registry failed/);await broken.get('session_shutdown')({},context);
