// purpose: verify the real Pi extension with the structural public host API and no npm packages.
// usage: node --experimental-strip-types pi-bridge.mjs (invoked by Rust cleanroom tests).
// effects: temporary Unix sockets and loopback HTTP only; all resources close in finally.
// requires: Node.js with type stripping and isolated HOME/CITO_STATE_DIR.
import assert from 'node:assert/strict';
import {createConnection} from 'node:net';
import {createServer} from 'node:http';
import {mkdirSync, writeFileSync, existsSync, statSync} from 'node:fs';
import {execFileSync} from 'node:child_process';
import {join} from 'node:path';
import bridge from '../../../../plugins/pi/cito-bridge.ts';
const state=process.env.CITO_STATE_DIR, path=join(state,'sockets',`pi-pid-${process.pid}.sock`);
const handlers=new Map(), messages=[], badges=[], notices=[], registrations=[], calls=[];
let session='session-a', renderer, command, attempts=0, failSend=false;
const context={cwd:'/repo',sessionManager:{getSessionId:()=>session,getSessionName:()=> 'Work'},ui:{setStatus:(_k,v)=>badges.push(v),notify:(s,t)=>notices.push({s,t})}};
const api={on:(n,f)=>handlers.set(n,f),registerMcpServer:(name,config)=>{if(++attempts===1)throw Error('Registry not ready');registrations.push({name,config});},registerMessageRenderer:(_n,r)=>{renderer=r},registerCommand:(_n,c)=>{command=c},sendMessage:(m,o)=>{if(failSend)throw Error('Retry');messages.push({m,o});}};
function request(value,fragment=false){return new Promise((resolve,reject)=>{const s=createConnection(path);let data='';s.setTimeout(2000,()=>s.destroy(Error('Timeout')));s.on('error',reject);s.on('connect',()=>{const raw=JSON.stringify(value)+'\n';if(fragment){s.write(raw.slice(0,5));s.end(raw.slice(5));}else s.write(raw);});s.on('data',c=>{data+=c;if(data.includes('\n')){s.destroy();resolve(JSON.parse(data));}});});}
const server=createServer(async(req,res)=>{let body='';for await(const chunk of req)body+=chunk;calls.push(JSON.parse(body));res.end(JSON.stringify({jsonrpc:'2.0',id:1,result:{content:[{type:'text',text:JSON.stringify({ok:true,listener_id:'alice-agent',scope:'self'})}]}}));});
try {
 bridge(api);assert.equal(existsSync(path),false);
 mkdirSync(join(state,'sockets'),{recursive:true});execFileSync('python3',['-c','import socket,sys; s=socket.socket(socket.AF_UNIX); s.bind(sys.argv[1]); s.close()',path]);
 await handlers.get('session_start')({},context);await handlers.get('session_start')({},context);
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
 for(let width=1;width<=12;width++)for(const line of narrow.render(width)){const cells=Array.from(line.replaceAll('\u4e2d','XX').replaceAll('👩‍💻','XX').replaceAll('é','e').replaceAll('🔔','XX')).length;assert(cells<=width,`width ${width}: ${line}`);}
 let c=0;const theme={fg:(_n,t)=>{c++;return t},bg:(_n,t)=>t,bold:t=>t};const card=renderer({content:[{type:'text',text:'hello café 👩‍💻 é'}]},{expanded:false},theme);assert(card.render(12).some(s=>s.includes('hello')));assert.deepEqual(card.render(0),[]);const before=c;card.invalidate();card.render(10);assert(c>before);
 await new Promise(r=>server.listen(0,'127.0.0.1',r));const port=server.address().port;writeFileSync(join(state,'config.json'),JSON.stringify({daemonPort:port}));process.env.CITO_DAEMON_PORT='1';
 for(const arg of ['on --hub work Review','team --hub=work Pair','off --hub work'])await command.handler(arg,context);
 assert.deepEqual(calls.map(v=>v.params.arguments),[{about:'Review',scope:'self',hub:'work'},{about:'Pair',scope:'workspace',hub:'work'},{hub:'work'}]);assert(calls.every(v=>v.params._meta.endpoint_pid===process.pid));
 process.env.CITO_DAEMON_PORT=String(port);writeFileSync(join(state,'config.json'),JSON.stringify({daemonPort:false}));await command.handler('on',context);assert.equal(calls.length,4);
 await command.handler('on --hub INVALID!',context);assert.equal(calls.length,4);assert.equal(notices.at(-1).t,'warning');
 assert.deepEqual(command.getArgumentCompletions('t'),[{value:'team',label:'team'}]);
} finally {await handlers.get('session_shutdown')?.({},context);server.closeAllConnections();if(server.listening)await new Promise(r=>server.close(r));}
assert.equal(existsSync(path),false);
const broken=new Map();bridge({...api,on:(n,f)=>broken.set(n,f),registerMcpServer:()=>{throw Error('Registry failed')}});await assert.rejects(()=>broken.get('session_start')({},context),/Registry failed/);await broken.get('session_shutdown')({},context);
