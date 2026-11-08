import {expect,test} from "vitest";
import {DatabaseSync} from "node:sqlite";
import {readFile,writeFile,stat,copyFile,mkdir} from "node:fs/promises";
import {join} from "node:path";
import {cleanroom,rows,socketServer,action,sendUnbound} from "../support/native-scenarios.js";
import {object,waitFor,type NativeHarness} from "../support/native-harness.js";
const threadId="01234567-89ab-4cde-8123-0123456789ab";
async function desktopProcess(f:NativeHarness){const executable=join(f.root,"Codex.app/Contents/MacOS/Codex");await mkdir(join(executable,".."),{recursive:true});await writeFile(executable,"setTimeout(()=>{},30000);\n");return f.spawn(process.execPath,[executable]);}
function database(f:NativeHarness,archived=0){const db=new DatabaseSync(join(f.env.CITO_CODEX_HOME!,"state_5.sqlite"));db.exec("CREATE TABLE threads(id TEXT PRIMARY KEY,name TEXT,title TEXT,cwd TEXT,git_origin_url TEXT,git_branch TEXT,archived INTEGER,updated_at INTEGER,thread_source TEXT,source TEXT)");db.prepare("INSERT INTO threads VALUES (?,?,?,?,?,?,?,100,'user',NULL)").run(threadId,"Session name","Thread title",f.root,"https://github.com/Owner/Repo.git","topic",archived);return db;}
const resolve=(f:NativeHarness,meta:Record<string,unknown>,patch:Record<string,unknown>={})=>f.plugin("codex",{op:"resolve_caller",cwd:f.root,meta,...patch});
test.each([{ "x-codex-turn-metadata": JSON.stringify({thread_id:threadId})},{"x-codex-turn-metadata":{session_id:threadId}},{"openai/threadId":" "+threadId+" "},{codexThreadId:threadId},{expected_harness:"codex",expected_session_id:threadId}])("extracts explicit thread claim %j",async meta=>{const f=await cleanroom();expect((await resolve(f,meta)).caller).toMatchObject({session_id:threadId,harness:"codex"});});
test("guards expected_session_id when an explicit socket PID is present",async()=>{const f=await cleanroom();expect((await resolve(f,{expected_harness:"codex",expected_session_id:threadId,endpoint_pid:process.pid})).ok).toBe(false);});
test("validates all explicit claims before state access",async()=>{const f=await cleanroom();for(const meta of [{codexThreadId:"a","openai/threadId":"b"},{codexThreadId:"a",pi_pid:123},{endpoint_pid:1,caller_pid:2}])expect((await resolve(f,meta)).error_code).toBe("INVALID_INPUT");await expect(stat(join(f.env.CITO_CODEX_HOME!,"state_5.sqlite"))).rejects.toMatchObject({code:"ENOENT"});});
test("merges HTTP/body thread claims and rejects conflicting aliases and explicit bridge PIDs",async()=>{
 const f=await cleanroom();const headers={"x-codex-turn-metadata":JSON.stringify({thread_id:threadId})};expect((await resolve(f,{}, {headers})).caller).toMatchObject({session_id:threadId});
 for(const meta of [{codexThreadId:"other"},{"openai/threadId":"other"},{"x-codex-turn-metadata":{session_id:"other"}},{endpoint_pid:1},{caller_pid:1}])expect((await resolve(f,meta,{headers})).error_code).toBe("INVALID_INPUT");
});
test("reads exact SQLite row, keeps background threads open, and never contacts Desktop IPC",async()=>{
 const f=await cleanroom(),db=database(f);let connections=0;const ipc=await socketServer(join(f.env.CITO_CODEX_HOME!,"ipc/ipc.sock"),s=>{connections++;s.destroy();});
 try{expect((await resolve(f,{codexThreadId:threadId})).caller).toMatchObject({session_id:threadId,cwd:f.root,title:"Session name",repo:"Owner/Repo",branch:"topic"});expect(connections).toBe(0);db.exec("UPDATE threads SET archived=1");expect((await resolve(f,{codexThreadId:threadId})).error_code).toBe("SESSION_NOT_OPEN");expect((await resolve(f,{codexThreadId:threadId},{require_open:false})).caller).toMatchObject({cwd:f.root});}finally{db.close();await ipc.close();}
});
test("missing or incompatible databases stay optional and are never created",async()=>{const f=await cleanroom();expect((await resolve(f,{codexThreadId:threadId})).caller).toMatchObject({session_id:threadId});await expect(readFile(join(f.env.CITO_CODEX_HOME!,"state_5.sqlite"))).rejects.toMatchObject({code:"ENOENT"});const db=new DatabaseSync(join(f.env.CITO_CODEX_HOME!,"state_5.sqlite"));try{expect((await resolve(f,{codexThreadId:threadId})).caller).toMatchObject({cwd:f.root});}finally{db.close();}});
test("busy SQLite metadata is unavailable and falls through to the explicit cwd",async()=>{const f=await cleanroom(),db=database(f);try{db.exec("PRAGMA journal_mode=DELETE;BEGIN EXCLUSIVE");expect((await resolve(f,{codexThreadId:threadId})).caller).toMatchObject({cwd:f.root});}finally{db.exec("ROLLBACK");db.close();}});
test("fallback order retains explicit cwd and does not borrow an unrelated workspace",async()=>{const f=await cleanroom();const custom=join(f.root,"explicit");expect((await resolve(f,{codexThreadId:threadId},{cwd:custom})).caller).toMatchObject({cwd:custom});const db=database(f);try{expect((await resolve(f,{codexThreadId:threadId},{cwd:null})).caller).toMatchObject({cwd:f.root});}finally{db.close();}});
test("probe archives definitively and uses real desktop liveness when metadata is unavailable",async()=>{const f=await cleanroom(),db=database(f,1);try{expect(await f.plugin("codex",{op:"probe",session_id:threadId})).toMatchObject({status:"closed_definitive"});}finally{db.close();}});
test("executes queue --resume with literal argv and CODEX_HOME, classifying insertion failures",async()=>{
 const f=await cleanroom(),log=join(f.root,"argv"),bin=join(f.root,"fake-codex");await writeFile(bin,`#!${process.execPath}\nrequire('node:fs').writeFileSync(${JSON.stringify(log)},JSON.stringify([process.argv.slice(2),process.env.CODEX_HOME]));`,{mode:0o700});const card='quoted "card"; $(never)\nline two';
 const r=await f.plugin("codex",{op:"wake",session_id:threadId,text:card,env:{...f.env,CITO_CODEX_BIN:bin}});expect.soft(r.outcome).toBe("delivered");
 const recovered=await f.plugin("codex",{op:"wake",session_id:threadId,text:card,queue_written:true,env:{...f.env,CITO_CODEX_BIN:bin}});expect(recovered.outcome).toBe("delivered_unconfirmed");expect(r.ok).toBe(true);expect(JSON.parse(await readFile(log,"utf8"))).toEqual([["queue","--resume","--thread",threadId,"--message",card],f.env.CITO_CODEX_HOME]);
 const bad=await f.plugin("codex",{op:"wake",session_id:threadId,text:card,env:{...f.env,CITO_CODEX_BIN:join(f.root,"missing")}});expect(bad.outcome).toBe("driver_unavailable");
});
test("resolves thread IDs from SQLite by recency, cwd, archive state and excludes subagents",async()=>{
 const f=await cleanroom(),db=database(f);try{db.prepare("INSERT INTO threads VALUES(?,?,?,?,?,?,?,500,'user',NULL)").run("newer","New",null,f.root,null,null,0);expect((await resolve(f,{})).caller).toMatchObject({session_id:"newer"});db.exec("UPDATE threads SET archived=1");expect((await resolve(f,{})).ok).toBe(false);db.exec("UPDATE threads SET archived=0;UPDATE threads SET thread_source='subagent' WHERE id='newer'");expect((await resolve(f,{})).caller).toMatchObject({session_id:threadId});}finally{db.close();}
});
test("discovers open user threads, excludes subagents/active listeners and does not starve other harnesses",async()=>{
 const f=await cleanroom(),db=database(f);const desktop=await desktopProcess(f);try{for(let i=1;i<=10;i++)db.prepare("INSERT INTO threads VALUES(?,?,?,?,?,?,?,?,'user',NULL)").run("codex-u-"+i,"Thread "+i,null,f.root,null,null,0,i*1000);db.prepare("INSERT INTO threads VALUES('sub','Subagent',NULL,?,NULL,NULL,0,99999,'subagent',NULL)").run(f.root);
 const discovered=rows((await f.plugin("codex",{op:"discover_sessions",endpoint_pid:desktop.child.pid})).sessions);expect(discovered).toHaveLength(8);expect(discovered[0]).toMatchObject({session_id:"codex-u-10"});expect(discovered.some(r=>r.session_id==="sub")).toBe(false);
 await f.daemon();await action(f,{op:"listen_session",harness:"codex",session_id:"codex-u-10",cwd:f.root});const snap=await action(f,{op:"get_snapshot"});expect(rows(snap.available).some(r=>r.session_id==="codex-u-10")).toBe(false);
 }finally{db.close();}
});

test("native auto-resolution preserves rowid/updated-at ties, cwd priority, archives and both subagent encodings",async()=>{
 const f=await cleanroom(),db=new DatabaseSync(join(f.env.CITO_CODEX_HOME!,"state_5.sqlite"));const lookup=(cwd=f.root,require_open=true)=>resolve(f,{}, {cli_cmd:require_open?"listen":"unlisten",cwd,require_open});
 try{db.exec("CREATE TABLE threads(id TEXT PRIMARY KEY,cwd TEXT,archived INTEGER)");for(const [id,cwd]of [["cwd-1",f.root],["cwd-2",f.root],["other-3",join(f.root,"other")]] as const)db.prepare("INSERT INTO threads VALUES(?,?,0)").run(id,cwd);expect((await lookup()).caller).toMatchObject({session_id:"cwd-2"});expect((await lookup(join(f.root,"unmatched"))).caller).toMatchObject({session_id:"other-3"});db.exec("DROP TABLE threads;CREATE TABLE threads(id TEXT PRIMARY KEY,cwd TEXT,archived INTEGER,updated_at INTEGER)");for(const [id,updated]of [["newer-time",500],["newer-row",100],["tie-newer-row",500]] as const)db.prepare("INSERT INTO threads VALUES(?,?,0,?)").run(id,f.root,updated);expect((await lookup()).caller).toMatchObject({session_id:"tie-newer-row"});db.exec("UPDATE threads SET archived=1");expect((await lookup()).error_code).toBe("BINDING_REQUIRED");expect((await lookup(f.root,false)).caller).toMatchObject({session_id:"tie-newer-row"});db.exec("DROP TABLE threads;CREATE TABLE threads(id TEXT PRIMARY KEY,cwd TEXT,archived INTEGER,thread_source TEXT,source TEXT,recency_at_ms INTEGER,updated_at INTEGER)");for(const [id,source,json,recency]of [["column-sub","subagent",null,900],["json-sub","user",JSON.stringify({subagent:"review"}),800],["null-user",null,null,700]] as const)db.prepare("INSERT INTO threads VALUES(?,?,0,?,?,?,?)").run(id,f.root,source,json,recency,recency);expect((await lookup()).caller).toMatchObject({session_id:"null-user"});}finally{db.close();}
});
test("native HTTP metadata reconciles header arrays and body fragments before reading SQLite",async()=>{
 const f=await cleanroom();const header=JSON.stringify({thread_id:threadId});for(const [meta,headers]of [[{},{"x-codex-turn-metadata":[header,JSON.stringify({session_id:"other"})]}],[{"x-codex-turn-metadata":{thread_id:threadId}},{"x-codex-turn-metadata":JSON.stringify({session_id:"other"})}]])expect((await resolve(f,meta!,{headers})).error_code).toBe("INVALID_INPUT");expect((await resolve(f,{"x-codex-turn-metadata":{thread_id:threadId}}, {headers:{"x-codex-turn-metadata":JSON.stringify({trace_id:"header"})}})).caller).toMatchObject({session_id:threadId});await expect(stat(join(f.env.CITO_CODEX_HOME!,"state_5.sqlite"))).rejects.toMatchObject({code:"ENOENT"});
});
test("Desktop open retries never repeat a successful native Codex queue insertion",async()=>{
 const f=await cleanroom(),desktop=await desktopProcess(f),db=database(f),q=join(f.root,"queue-count"),o=join(f.root,"open-lines"),driver=join(f.root,"queue-driver"),open=join(f.root,"guard-bin/open");await writeFile(driver,'#!/bin/sh\nprintf "queue\\n" >> '+JSON.stringify(q)+'\n',{mode:0o700});await writeFile(open,'#!/bin/sh\nprintf "%s\\n" "$#|$1|$2" >> '+JSON.stringify(o)+'\nn=$(wc -l < '+JSON.stringify(o)+'); [ "$n" -gt 1 ]\n',{mode:0o700});try{await f.daemon({CITO_CODEX_BIN:driver});const meta={codexThreadId:threadId,cwd:f.root},listen=await f.tool("cito_listen",{about:"queue once"},meta);await sendUnbound(f,String(listen.listener_id));await waitFor(async()=>readFile(o,"utf8").then(v=>v.trim().split("\n").length>=2,()=>false),5000);expect((await readFile(q,"utf8")).trim().split("\n")).toHaveLength(1);expect((await readFile(o,"utf8")).trim().split("\n")).toEqual(["2|-g|codex://threads/wake/"+threadId,"2|-g|codex://threads/wake/"+threadId]);await waitFor(()=>f.query("SELECT state FROM local_signals")[0]?.state==="DISPATCHED");expect(f.query("SELECT state,queue_written,acknowledged_at_ms FROM local_signals")[0]).toEqual({state:"DISPATCHED",queue_written:1,acknowledged_at_ms:expect.any(Number)});expect((await f.tool("cito_inbox",{},meta)).signals).toHaveLength(0);}finally{db.close();}
},10000);
test("native Codex queue timeout records unknown insertion, submits one open and remains unread",async()=>{
 const f=await cleanroom(),desktop=await desktopProcess(f),db=database(f),driver=join(f.root,"queue-timeout"),q=join(f.root,"queue-count"),o=join(f.root,"open-lines");await writeFile(driver,'#!/bin/sh\nprintf "queue\\n" >> '+JSON.stringify(q)+'\nexec /bin/sleep 30\n',{mode:0o700});await writeFile(join(f.root,"guard-bin/open"),'#!/bin/sh\nprintf "%s\\n" "$#|$1|$2" >> '+JSON.stringify(o)+'\n',{mode:0o700});try{await f.daemon({CITO_CODEX_BIN:driver});const meta={codexThreadId:threadId,cwd:f.root},listen=await f.tool("cito_listen",{about:"unknown insertion"},meta);const sending=sendUnbound(f,String(listen.listener_id)).catch(e=>e);await waitFor(()=>f.query("SELECT state FROM local_signals")[0]?.state==="DISPATCHED",13000);await sending;expect((await readFile(q,"utf8")).trim().split("\n")).toEqual(["queue"]);expect((await readFile(o,"utf8")).trim().split("\n")).toEqual(["2|-g|codex://threads/wake/"+threadId]);expect(f.query("SELECT queue_written,acknowledged_at_ms FROM local_signals")[0]).toEqual({queue_written:1,acknowledged_at_ms:null});expect((await f.tool("cito_inbox",{},meta)).signals).toHaveLength(1);}finally{db.close();}
},20000);

test("three definite native queue failures never submit an open",async()=>{
 const f=await cleanroom(),driver=join(f.root,"failed-queue"),q=join(f.root,"queue-attempts"),o=join(f.root,"open-attempts");
 await writeFile(driver,'#!/bin/sh\nprintf "queue\\n" >> '+JSON.stringify(q)+'\nexit 2\n',{mode:0o700});
 await writeFile(join(f.root,"guard-bin/open"),'#!/bin/sh\nprintf "open\\n" >> '+JSON.stringify(o)+'\n',{mode:0o700});
 expect(await f.plugin("codex",{op:"wake",session_id:threadId,text:"card",env:{...f.env,CITO_CODEX_BIN:driver}})).toMatchObject({outcome:"driver_unavailable",queue_written:false});
 expect((await readFile(q,"utf8")).trim().split("\n")).toEqual(["queue","queue","queue"]);await expect(stat(o)).rejects.toMatchObject({code:"ENOENT"});
},10000);
test.each(["ack","unlisten","fence"])("%s during a real native queue prevents the separately fenced open",async boundary=>{
 const f=await cleanroom(),desktop=await desktopProcess(f),db=database(f),driver=join(f.root,"blocked-queue"),marker=join(f.root,"queue-entered"),release=join(f.root,"release"),o=join(f.root,"open-attempts");
 await writeFile(driver,'#!/bin/sh\nprintf "entered" > '+JSON.stringify(marker)+'\nwhile [ ! -e '+JSON.stringify(release)+' ]; do /bin/sleep 0.01; done\n',{mode:0o700});
 await writeFile(join(f.root,"guard-bin/open"),'#!/bin/sh\nprintf "open\\n" >> '+JSON.stringify(o)+'\n',{mode:0o700});
 try{
  await f.daemon({CITO_CODEX_BIN:driver});const meta={codexThreadId:threadId,cwd:f.root},listener=await f.tool("cito_listen",{about:"In-flight boundary"},meta);
  await sendUnbound(f,String(listener.listener_id));await waitFor(()=>stat(marker).then(()=>true,()=>false));
  if(boundary==="ack")await f.tool("cito_inbox",{ack_all:true},meta);
  else if(boundary==="unlisten")await f.tool("cito_unlisten",{},meta);
  else {
   const row=f.query("SELECT listener_id FROM local_listeners WHERE harness='codex'")[0]!;
   const remote=object(JSON.parse(String(f.query("SELECT CAST(value AS TEXT) AS payload FROM hub_kv WHERE key=?",[String(row.listener_id)],"hub.sqlite")[0]!.payload)));
   remote.binding_generation=Number(remote.binding_generation)+1;remote.owner_token="12345678-1234-4234-8234-123456789012";
   f.mutate("UPDATE hub_kv SET value=CAST(? AS BLOB) WHERE key=?",[JSON.stringify(remote),String(row.listener_id)],"hub.sqlite");
  }
  await writeFile(release,"release");await waitFor(()=>f.query("SELECT state FROM local_signals")[0]?.state==="STORED_PASSIVE");
  if(boundary==="ack")expect(f.query("SELECT queue_written,acknowledged_at_ms FROM local_signals")[0]).toEqual({queue_written:1,acknowledged_at_ms:expect.any(Number)});
  if(boundary==="fence")expect(f.query("SELECT status FROM local_listeners WHERE harness='codex'")[0]!.status).toBe("FENCED_INACTIVE");
  await new Promise(r=>setTimeout(r,350));await expect(stat(o)).rejects.toMatchObject({code:"ENOENT"});
 }finally{db.close();}
},10000);

test("unavailable Codex metadata uses actual owned desktop process liveness without creating a database",async()=>{
 const f=await cleanroom(),desktop=await desktopProcess(f),other=await f.spawn("/bin/sleep",["30"]);expect(await f.plugin("codex",{op:"probe",session_id:threadId,endpoint_pid:desktop.child.pid})).toMatchObject({status:"alive"});expect(await f.plugin("codex",{op:"probe",session_id:threadId,endpoint_pid:other.child.pid})).toMatchObject({status:"alive"});expect(rows((await f.plugin("codex",{op:"discover_sessions",endpoint_pid:other.child.pid})).sessions)).toEqual([]);await expect(stat(join(f.env.CITO_CODEX_HOME!,"state_5.sqlite"))).rejects.toMatchObject({code:"ENOENT"});
});
