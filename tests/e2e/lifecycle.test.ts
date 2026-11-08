import {expect,test} from "vitest";
import {DatabaseSync} from "node:sqlite";
import {writeFile,readFile} from "node:fs/promises";
import {join} from "node:path";
import {agent,remoteAgent,cleanroom,provider,action,sendUnbound,received,rows,errorTool,seed,human} from "../support/native-scenarios.js";
import {waitFor,object} from "../support/native-harness.js";
import {SESSION,transcript,sidecar,request,queue} from "../support/plugin-fixtures.js";

test("confirmed Pi delivery automatically drains its inbox and refreshes the badge",async()=>{
 const f=await cleanroom(),pi=await f.piBridge();await f.daemon();const r=await f.tool("cito_listen",{about:"Pi receiver"},pi.meta);const sent=await sendUnbound(f,String(r.listener_id));await waitFor(()=>pi.messages.length===1);await waitFor(()=>typeof f.query("SELECT acknowledged_at_ms FROM local_signals WHERE signal_id=?",[String(sent.signal_id)])[0]?.acknowledged_at_ms==="number");expect((await f.tool("cito_inbox",{},pi.meta)).signals).toEqual([]);await waitFor(()=>!String(pi.badges.at(-1)??"").includes("(1)"));
});
test("Codex lifecycle and offline post-close inbox/unlisten use explicit metadata and retained state",async()=>{
 const f=await cleanroom();const db=new DatabaseSync(join(f.env.CITO_CODEX_HOME!,"state_5.sqlite"));db.exec("CREATE TABLE threads(id TEXT PRIMARY KEY,title TEXT,cwd TEXT,archived INTEGER)");db.prepare("INSERT INTO threads VALUES('thread','Codex',?,0)").run(f.root);const driver=join(f.root,"fake-codex");await writeFile(driver,'#!/bin/sh\nexit 0\n',{mode:0o700});await f.daemon({CITO_CODEX_BIN:driver,CITO_OPEN_BIN:join(f.root,"guard-bin/open")});const meta={codexThreadId:"thread",cwd:f.root};const listen=await f.tool("cito_listen",{about:"Codex"},meta);await sendUnbound(f,String(listen.listener_id));db.exec("UPDATE threads SET archived=1");db.close();const inbox=await f.tool("cito_inbox",{},meta);expect(inbox.signals).toHaveLength(1);expect((await f.tool("cito_unlisten",{},meta)).status).toBe("unlistened");
});
test("offline Pi inbox reads and acknowledges SQLite without contacting a Hub",async()=>{
 const owner=await cleanroom(),hub=await owner.hub(),f=await cleanroom({hubs:{work:{url:hub.url,authMode:"none"}},defaultHub:"work"}),pi=await f.piBridge();await f.daemon();await f.tool("cito_listen",{about:"offline"},pi.meta);const row=f.query("SELECT * FROM local_listeners WHERE harness='pi'")[0]!;f.mutate("INSERT INTO local_signals(hub,signal_id,listener_id,seq,binding_generation,from_user,from_host_name,from_title,message,created_at,expires_at_ms,state) VALUES('work','offline',?,1,?,'alice','other','','offline','now',?,'STORED_PASSIVE')",[String(row.listener_id),Number(row.binding_generation),Date.now()+60000]);await hub.process.stop();expect((await f.tool("cito_inbox",{},pi.meta)).signals).toHaveLength(1);expect(typeof f.query("SELECT acknowledged_at_ms FROM local_signals WHERE signal_id='offline'")[0]!.acknowledged_at_ms).toBe("number");
});
test("same-channel takeover fences the old owner without mutating the winner's new signals",async()=>{
 const owner=await cleanroom(),hub=await owner.hub(),config={hubs:{work:{url:hub.url,authMode:"none"}},defaultHub:"work"};const a=await cleanroom(config),pa=await provider(a);await a.daemon();const old=await a.tool("cito_listen",{about:"old"},pa.meta);const host=await readFile(join(a.stateDir,"host-id"),"utf8");const b=await cleanroom(config),pb=await provider(b);await writeFile(join(b.stateDir,"host-id"),host);await b.daemon();const winner=await b.tool("cito_listen",{about:"winner"},pb.meta);await sendUnbound(b,String(winner.listener_id));await waitFor(()=>a.query("SELECT status FROM local_listeners WHERE harness='cursor'")[0]!.status==="FENCED_INACTIVE");expect(b.query("SELECT status FROM local_listeners WHERE harness='cursor'")[0]!.status).toBe("ACTIVE");expect(winner.binding_generation).toBeGreaterThan(Number(old.binding_generation));
});
test("acknowledgment prevents a debounced wake and unlisten cancels admitted batches",async()=>{
 const {f,p,row,id}=await agent();seed(f,row,"ack-before-wake");await f.tool("cito_inbox",{},p.meta);await new Promise(r=>setTimeout(r,400));expect((await p.calls()).filter(c=>c.op==="wake")).toEqual([]);seed(f,row,"cancel",2);await f.tool("cito_unlisten",{},p.meta);await new Promise(r=>setTimeout(r,400));expect(f.query("SELECT state FROM local_signals WHERE signal_id='cancel'")[0]!.state).toBe("STORED_PASSIVE");
});
test("liveness sweep is single-flight and revokes switched sessions",async()=>{
 const {f,p,id}=await agent({}, {delay_op:"probe",delay_ms:250});await p.set({status:"switched",session_id:"other"});await waitFor(()=>f.query("SELECT status FROM local_listeners WHERE listener_id=?",[id])[0]!.status==="UNLISTENED");expect((await p.calls()).filter(c=>c.op==="probe")).toHaveLength(1);
});
test("private targets are invisible/forbidden cross-user and workspace peers can wake",async()=>{
 const owner=await cleanroom(),hub=await owner.hub(),config={hubs:{work:{url:hub.url,authMode:"none",user:"alice"}},defaultHub:"work"};const a=await cleanroom(config),p=await provider(a);await a.daemon();const target=await a.tool("cito_listen",{about:"private",scope:"self"},p.meta);const b=await cleanroom({hubs:{work:{url:hub.url,authMode:"none",user:"bob"}},defaultHub:"work"});await b.daemon();expect(rows((await b.tool("cito_find",{harness:"cursor"})).listeners)).toEqual([]);expect((await errorTool(b,"cito_send",{to:target.listener_id,message:"private"},human)).error).toMatchObject({code:"FORBIDDEN_TARGET_SCOPE"});const publicTarget=await a.tool("cito_listen",{about:"workspace",scope:"workspace"},p.meta);await sendUnbound(b,String(publicTarget.listener_id),"workspace");await waitFor(()=>a.query("SELECT state FROM local_signals WHERE message='workspace'")[0]?.state==="DISPATCHED");const local=a.query("SELECT state,held_reason FROM local_signals WHERE message='workspace'")[0]!;expect(local).toMatchObject({state:"DISPATCHED",held_reason:null});expect((await p.calls()).filter(c=>c.op==="wake").flatMap(c=>rows(c.signals))).toMatchObject([{message:"workspace",from_user:"bob"}]);
});
test("find combines filters and inbox cannot acknowledge a different session",async()=>{
 const {f,p,enrolled,row}=await agent({}, {repo:"acme/api",branch:"main"});const found=rows((await f.tool("cito_find",{user:"alice",harness:"cursor",repo:"acme/api",host:String(row.host_name),query:"native"})).listeners);expect(found).toHaveLength(1);expect(rows((await f.tool("cito_find",{harness:"aider"})).listeners)).toEqual([]);seed(f,row,"own");await p.set({session_id:"other"});await f.tool("cito_listen",{about:"other"},{expected_harness:"cursor",expected_session_id:"other"});await f.tool("cito_inbox",{ack_ids:["own"]},{expected_harness:"cursor",expected_session_id:"other"});expect(f.query("SELECT acknowledged_at_ms FROM local_signals WHERE signal_id='own'")[0]!.acknowledged_at_ms).toBeNull();
});
test("PENDING_PUBLISH recovery reuses its exact signal ID and Hub dedup sequence",async()=>{
 const {f,proxy,enrolled,owner}=await remoteAgent();proxy.fail("js_publish",1);await sendUnbound(f,String(enrolled.listener_id),"recover",{idempotency_key:"same"}).catch(()=>{});const row=f.query("SELECT * FROM local_sent_idempotency")[0]!;expect(row.status).toBe("PENDING_PUBLISH");proxy.restore();const recovered=await sendUnbound(f,String(enrolled.listener_id),"recover",{idempotency_key:"same"});expect(recovered.signal_id).toBe("work:"+String(row.signal_id));expect(owner.query("SELECT COUNT(*) AS n FROM hub_signals WHERE signal_id=?",[String(row.signal_id)],"hub/hub.sqlite")[0]!.n).toBe(1);
});
test("native Claude channel preserves first-turn aliases and hook acknowledgments use exact batch IDs",async()=>{
 const f=await cleanroom();await f.daemon();const channel=await sidecar(f);
 try{
  const first=await request(channel.path,{op:"identify"}),sid=String(first.session_id);
  const listener=await action(f,{op:"listen_session",harness:"claude",session_id:sid,endpoint_pid:process.pid,cwd:f.root,about:"Claude"});
  await transcript(f,SESSION);expect((await request(channel.path,{op:"identify"})).session_id).toBe(sid);
  const sent=await sendUnbound(f,String(listener.listener_id),"card");
  await waitFor(async()=>(await queue(f,sid)).length===1);
  const queued=(await queue(f,sid))[0]!,signal=f.query("SELECT * FROM local_signals WHERE signal_id=?",[String(sent.signal_id)])[0]!;
  expect(queued.batch_id).toBe(signal.batch_id);expect(signal.acknowledged_at_ms).toBeNull();
  const row=f.query("SELECT * FROM local_listeners WHERE harness='claude'")[0]!;
  seed(f,row,"not-drained",Number(signal.seq)+1,{state:"STORED_PASSIVE",held_reason:"deferred"});
  const hook=await f.command([],{executable:"plugins/claude/bin/cito-claude-hook",input:JSON.stringify({hook_event_name:"Stop",session_id:SESSION,cwd:f.root})});
  expect(JSON.parse(hook.stdout)).toMatchObject({decision:"block",reason:expect.stringContaining("card")});
  expect(await queue(f,sid)).toEqual([]);
  await waitFor(()=>typeof f.query("SELECT acknowledged_at_ms FROM local_signals WHERE signal_id=?",[String(sent.signal_id)])[0]?.acknowledged_at_ms==="number");
  expect(f.query("SELECT acknowledged_at_ms FROM local_signals WHERE signal_id='not-drained'")[0]!.acknowledged_at_ms).toBeNull();
 }finally{await channel.close();}
},10000);
test("plain stdio discovers its parent Pi bridge and exits cleanly on SIGINT/SIGTERM",async()=>{
 const f=await cleanroom();await f.piBridge();await f.daemon();for(const signal of ["SIGINT","SIGTERM"] as const){const child=await f.spawn("bin/cito-mcp");child.child.stdin.write(JSON.stringify({jsonrpc:"2.0",id:1,method:"tools/call",params:{name:"cito_listen",arguments:{about:"signal"}}})+"\n");await waitFor(()=>child.stdout.includes('"id":1'));expect(child.stdout).toContain("pi-session-1");child.child.kill(signal);expect((await child.result).code).toBe(0);}
});
