import {expect,test} from "vitest";
import {mkdir,writeFile,stat} from "node:fs/promises";
import {join} from "node:path";
import {agent,cleanroom,errorTool,provider,rows,socketServer} from "../support/native-scenarios.js";
import {bridgePath} from "../support/plugin-fixtures.js";
import {waitFor} from "../support/native-harness.js";

test("PID extraction validates explicit claims and uses nested roots without appending the parent",async()=>{
 const f=await cleanroom();await f.piBridge();for(const pi_pid of [0,-1,0.5,Number.MAX_SAFE_INTEGER+1,"12"]){const r=await f.plugin("pi",{op:"resolve_caller",meta:{pi_pid}});expect(r.ok).toBe(false);}
 expect((await f.plugin("pi",{op:"resolve_caller",meta:{nested:{parent_pid:123456}}})).ok).toBe(false);
});
test("a nested explicit root cannot accidentally bind to a bridge on process.ppid",async()=>{
 const f=await cleanroom();const s=await socketServer(bridgePath(f,"pi",process.ppid),socket=>socket.once("data",()=>socket.end(JSON.stringify({ok:true,session_id:"wrong",cwd:f.root})+"\n")));try{expect((await f.plugin("pi",{op:"resolve_caller",meta:{nested:{parent_pid:123456}}})).ok).toBe(false);}finally{await s.close();}
});
test("external identity hooks receive generic context and conflicting driver/PID claims fail",async()=>{
 const {f,p}=await agent();expect((await f.tool("cito_listen",{about:"again"},p.meta)).harness).toBe("cursor");const call=(await p.calls()).find(c=>c.op==="resolve_caller")!;expect(call).toMatchObject({meta:p.meta,state_dir:f.stateDir,require_open:true});
 expect((await errorTool(f,"cito_listen",{about:"bad"},{...p.meta,pi_pid:1,caller_pid:2})).error).toMatchObject({code:"INVALID_INPUT"});
});
test("native probes distinguish matching, switched, definitive closure and transient failure",async()=>{
 const f=await cleanroom();const pi=await f.piBridge("live");expect(await f.plugin("pi",{op:"probe",endpoint_pid:process.pid,session_id:"live"})).toMatchObject({status:"alive"});await pi.switchSession("new");expect(await f.plugin("pi",{op:"probe",endpoint_pid:process.pid,session_id:"live"})).toMatchObject({status:"switched",current_session_id:"new"});await pi.stop();expect(await f.plugin("pi",{op:"probe",endpoint_pid:process.pid,session_id:"live"})).toMatchObject({status:"closed_definitive"});
});
test("offline retained identity rejects zero/multiple pairs and accepts one exact session",async()=>{
 const {f,p,id}=await agent();await f.tool("cito_unlisten",{},p.meta);const result=await f.tool("cito_inbox",{},p.meta);expect(result.signals).toEqual([]);await p.set({caller_error:"BINDING_REQUIRED"});expect((await errorTool(f,"cito_inbox",{},p.meta)).error).toMatchObject({code:"BINDING_REQUIRED"});expect(f.query("SELECT listener_id FROM local_listeners WHERE harness='cursor'")[0]!.listener_id).toBe(id);
});
test("stateDir plugins enforce reserved IDs, directory-name match and rejected-plugin isolation",async()=>{
 const f=await cleanroom();await provider(f);for(const [dir,id]of [["pi","pi"],["mismatch","other"]]){const root=join(f.stateDir,"plugins",dir!);await mkdir(root,{recursive:true});await writeFile(join(root,"plugin.json"),JSON.stringify({id,kind:"agent-harness",command:["/usr/bin/false"]}));}
 await f.daemon();expect((await f.tool("cito_listen",{about:"external"},{expected_harness:"cursor",expected_session_id:"session-1"})).harness).toBe("cursor");expect((await errorTool(f,"cito_listen",{about:"reserved"},{expected_harness:"other",expected_session_id:"s"})).error).toMatchObject({code:"UNSUPPORTED_HARNESS"});
});
test("native one-shot deadlines isolate hung providers and only one stdio sidecar owns an endpoint",async()=>{
 const f=await cleanroom();const p=await provider(f,"cursor",{delay_op:"discover_sessions",delay_ms:30000});await f.daemon();const start=performance.now();await f.tool("cito_find",{});expect(performance.now()-start).toBeLessThan(10000);expect((await f.mcp("tools/list")).tools).toHaveLength(5);
});
test("CLI harness/session flags validate orphan sessions, unknown harnesses and conflicts",async()=>{
 const {f}=await agent();for(const args of [["listen","--session","s"],["listen","--harness","unknown","--session","s"],["listen","--pi","--codex"],["listen","--harness","cursor","--pi"]]){const result=await f.command(args);expect(result.code).toBe(1);expect(result.stderr).not.toBe("");}
});
