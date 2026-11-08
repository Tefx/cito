import { afterEach, expect } from "vitest";
import { mkdir, readFile, writeFile } from "node:fs/promises";
import { join } from "node:path";
import { createServer, type Socket } from "node:net";
import { nativeFixture, object, waitFor, type NativeHarness, type JsonObject } from "./native-harness.js";

const owned: NativeHarness[] = [];
afterEach(async () => { const results=await Promise.allSettled(owned.splice(0).reverse().map(f=>f.close()));const errors=results.filter(r=>r.status==="rejected").map(r=>r.reason);if(errors.length)throw new AggregateError(errors,"Native fixture cleanup failed"); });
export async function cleanroom(config: JsonObject = {}) { const f = await nativeFixture({config}); owned.push(f); return f; }
export const human = { expected_harness: "human", expected_session_id: "default" };
export const rows = (v: unknown): JsonObject[] => { if (!Array.isArray(v)) throw new Error("Expected array"); return v.map(x => object(x)); };
export async function action(f: NativeHarness, value: JsonObject, allowError = false, timeout=10_000): Promise<JsonObject> {
  const response = await fetch(`http://127.0.0.1:${f.daemonPort}/ui`, { method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify(value), signal: AbortSignal.timeout(timeout) });
  const result = object(await response.json());
  if (!response.ok && !allowError) throw new Error(JSON.stringify(result));
  return { ...result, http_status: response.status };
}
export const snapshot = (f: NativeHarness) => action(f, {op:"get_snapshot"});
export async function errorTool(f: NativeHarness,name:string,args:JsonObject={},meta:JsonObject={}) {
  const result=await f.mcp("tools/call",{name,arguments:args,_meta:meta});
  expect(result.isError).toBe(true);const content=rows(result.content)[0]!;return object(JSON.parse(String(content.text)));
}
export async function provider(f: NativeHarness, id="cursor", state: JsonObject = {}) {
  const dir=join(f.stateDir,"plugins",id); await mkdir(dir,{recursive:true});
  const data=join(dir,"fixture.json"),log=join(dir,"calls.jsonl");
  const initial={session_id:"session-1",cwd:f.root,title:"Native fixture",status:"alive",outcome:"delivered",endpoint_pid:process.pid,...state};
  await writeFile(data,JSON.stringify(initial));
  const script=`import fs from 'node:fs';import readline from 'node:readline';
const lines=readline.createInterface({input:process.stdin});
for await (const line of lines){const r=JSON.parse(line),s=JSON.parse(fs.readFileSync(${JSON.stringify(data)},'utf8'));fs.appendFileSync(${JSON.stringify(log)},JSON.stringify(r)+'\\n');
if(s.delay_op===r.op)await new Promise(y=>setTimeout(y,s.delay_ms??1000));
let v={ok:true};const c={harness:${JSON.stringify(id)},session_id:s.session_id,cwd:s.cwd,title:s.title,repo:s.repo??null,branch:s.branch??null,endpoint_pid:s.endpoint_pid??null};
if(r.op==='resolve_caller')v=s.caller_error?{ok:false,error_code:s.caller_error,message:s.caller_error}:(r.meta?.expected_harness===c.harness||s.implicit?{ok:true,caller:c}:{ok:false,error_code:'BINDING_REQUIRED',message:'Explicit fixture identity required'});
else if(r.op==='discover_sessions')v={ok:true,sessions:s.hidden?[]:[{...c,updated_at_ms:Date.now()}]};
else if(r.op==='probe')v={ok:true,status:s.status,current_session_id:s.session_id,reason:s.reason??null};
else if(r.op==='wake')v={ok:true,outcome:s.outcome,reason:s.reason??null};
else if(r.op==='setup_inspect')v={ok:true,installed:true,configured:false,paths:[]};
else if(r.op==='setup_apply'||r.op==='setup_remove')v={ok:true,actions:[]};
console.log(JSON.stringify(v));break;}
`;
  const driver=join(dir,"driver.mjs");await writeFile(driver,script);
  await writeFile(join(dir,"plugin.json"),JSON.stringify({id,kind:"agent-harness",displayName:"Fixture Agent",tier:"experimental",command:[process.execPath,driver]}));
  return {id,meta:{expected_harness:id,expected_session_id:String(initial.session_id)},async set(patch:JsonObject){Object.assign(initial,patch);await writeFile(data,JSON.stringify(initial));},async calls(){try{return (await readFile(log,"utf8")).trim().split("\n").filter(Boolean).map(x=>object(JSON.parse(x)));}catch(e){if((e as NodeJS.ErrnoException).code==="ENOENT")return [];throw e;}}};
}
export async function agent(config:JsonObject={},state:JsonObject={}) {
  const f=await cleanroom(config);const p=await provider(f,"cursor",state);const daemon=await f.daemon();
  const enrolled=await f.tool("cito_listen",{about:"Native receiver",scope:"workspace"},p.meta);
  const local=f.query("SELECT * FROM local_listeners WHERE harness='cursor'");expect(local).toHaveLength(1);
  return {f,p,daemon,enrolled,id:String(local[0]!.listener_id),hub:String(local[0]!.hub),row:local[0]!};
}
export async function sendUnbound(f:NativeHarness,to:string,message="Native signal",extra:JsonObject={}) {
  // Explicit Human identity keeps a controlled agent provider from attributing the sender to its recipient.
  return f.tool("cito_send",{to,message,...extra},human);
}
export async function received(f:NativeHarness,id:string,count=1) { await waitFor(()=>f.query("SELECT * FROM local_signals WHERE listener_id=?",[id]).length===count,10_000);return f.query("SELECT * FROM local_signals WHERE listener_id=? ORDER BY seq",[id]); }
export async function socketServer(path:string,handler:(socket:Socket)=>void){
  await mkdir(join(path,".."),{recursive:true});const sockets=new Set<Socket>();const server=createServer(s=>{sockets.add(s);s.on("error",()=>{});s.once("close",()=>sockets.delete(s));handler(s);});
  await new Promise<void>((y,n)=>{server.once("error",n);server.listen(path,y);});
  return {async close(){for(const s of sockets)s.destroy();await new Promise<void>((y,n)=>server.close(e=>e?n(e):y()));}};
}
export async function remoteAgent(state:JsonObject={}) {
 const owner=await cleanroom();const hub=await owner.hub();const {proxyHub}=await import("./hub-proxy.js");const proxy=await proxyHub(hub.url);
 const f=await cleanroom({hubs:{work:{url:proxy.url,authMode:"none",user:"alice"}},defaultHub:"work"});f.signalStores.set("work",{fixture:owner,database:"hub/hub.sqlite"});const p=await provider(f,"cursor",state);const daemon=await f.daemon();const enrolled=await f.tool("cito_listen",{about:"Remote receiver",scope:"workspace"},p.meta);const row=f.query("SELECT * FROM local_listeners WHERE harness='cursor'")[0]!;
 return {f,p,daemon,owner,hub,proxy,enrolled,row,id:String(row.listener_id)};
}
export function inject(f:NativeHarness,signal:JsonObject,seq:number,database="hub.sqlite") {f.mutate("INSERT INTO hub_signals(seq,subject,msg_id,signal_id,created_ms,data) VALUES(?,?,?,?,?,CAST(? AS BLOB))",[seq,"cito.signal."+String(signal.to),"wire-"+seq,String(signal.signal_id),Date.now(),JSON.stringify(signal)],database);}
export function wire(id:string,to:string,principal:string,generation=1,message="Native signal") {return {signal_id:id,to,target_generation:generation,sender_endpoint_key:"sender",from:{principal_id:principal,user:"alice",host_id:"other",host_name:"other",harness:null,listener_id:null,title:"Sender",about:null},message,in_reply_to:null};}
export function seed(f:NativeHarness,listener:JsonObject,id:string,seq=1,patch:JsonObject={}) {
  const v={...listener,...patch};const message=String(v.message??"Native signal"),user=String(v.from_user??"alice"),host=String(v.from_host_name??"other"),title=v.from_title===null?null:String(v.from_title??"Sender"),harness=v.from_harness===null?null:String(v.from_harness??"cursor");
  f.mutate(`INSERT INTO local_signals(hub,signal_id,listener_id,seq,binding_generation,from_user,from_host_name,from_harness,from_listener_id,from_title,message,created_at,expires_at_ms,state,held_reason,acknowledged_at_ms,batch_id,queue_written) VALUES(?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?)`,[String(v.hub),id,String(v.listener_id),seq,Number(v.binding_generation),user,host,harness,v.from_listener_id===null?null:String(v.from_listener_id??"sender"),title,message,String(v.created_at??new Date().toISOString()),Number(v.expires_at_ms??Date.now()+60_000),String(v.state??"RECEIVED"),v.held_reason==null?null:String(v.held_reason),v.acknowledged_at_ms==null?null:Number(v.acknowledged_at_ms),v.batch_id==null?null:String(v.batch_id),Number(v.queue_written??0)]);
  const payload:JsonObject=wire(id,String(v.listener_id),String(v.principal_id),Number(v.binding_generation),message);payload.from={...object(payload.from),user,host_name:host,title,harness,listener_id:v.from_listener_id as null};
  const store=f.signalStores.get(String(v.hub))??{fixture:f,database:"hub.sqlite"};store.fixture.mutate("INSERT OR IGNORE INTO hub_signals(seq,subject,msg_id,signal_id,created_ms,data) VALUES(?,?,?,?,?,CAST(? AS BLOB))",[seq,"cito.signal."+String(v.listener_id),id,id,Date.now(),JSON.stringify(payload)],store.database);
}
