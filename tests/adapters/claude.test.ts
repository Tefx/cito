import {expect,test} from "vitest";
import {mkdir,writeFile,readFile,stat,rename,readdir} from "node:fs/promises";
import {join} from "node:path";
import {cleanroom} from "../support/native-scenarios.js";
import {SESSION,OTHER,transcript,projectDir,queue,sidecar,request} from "../support/plugin-fixtures.js";

test("the newest transcript names the running session and unrelated files never do",async()=>{
 const f=await cleanroom();expect((await f.plugin("claude",{op:"resolve_caller",cwd:f.root})).ok).toBe(false);await transcript(f,OTHER,undefined,1700000000000);await transcript(f,SESSION,undefined,1800000000000);await writeFile(join(projectDir(f),"unrelated.txt"),"{}");
 const ambiguous=await f.plugin("claude",{op:"resolve_caller",cwd:f.root});expect(ambiguous.error_code).toBe("BINDING_REQUIRED");
 const explicit=await f.plugin("claude",{op:"resolve_caller",cwd:f.root,meta:{claudeSessionId:SESSION}});expect(explicit.caller).toMatchObject({session_id:SESSION});
});
test("the newest custom-title names the session, and an untitled or unreadable transcript has none",async()=>{
 const f=await cleanroom();await transcript(f,SESSION,"Original");await writeFile(join(projectDir(f),SESSION+".jsonl"),JSON.stringify({type:"custom-title",customTitle:"First"})+"\n"+JSON.stringify({type:"custom-title",customTitle:"Renamed"})+"\n");
 expect((await f.plugin("claude",{op:"resolve_caller",cwd:f.root,meta:{claudeSessionId:SESSION}})).caller).toMatchObject({title:"Renamed",session_id:SESSION});
 await transcript(f,SESSION);expect((await f.plugin("claude",{op:"resolve_caller",cwd:f.root,meta:{claudeSessionId:SESSION}})).caller).not.toMatchObject({title:"Renamed"});
});
test("only the transcript tail is read, so a title older than the window yields the workspace fallback",async()=>{
 const f=await cleanroom();await transcript(f,SESSION,"Old title");await writeFile(join(projectDir(f),SESSION+".jsonl"),JSON.stringify({type:"custom-title",customTitle:"Old title"})+"\n"+"{}\n".repeat(100000));const r=await f.plugin("claude",{op:"resolve_caller",cwd:f.root,meta:{claudeSessionId:SESSION}});expect(r.caller).not.toMatchObject({title:"Old title"});
});
test("explicit session claims accept one identifier, defer expected_session_id with a socket PID, and reject conflicts",async()=>{
 const f=await cleanroom();for(const key of ["claudeSessionId","claude_session_id","sessionId"]){const r=await f.plugin("claude",{op:"resolve_caller",cwd:f.root,meta:{[key]:SESSION}});expect(r.caller).toMatchObject({session_id:SESSION});}
 expect((await f.plugin("claude",{op:"resolve_caller",cwd:f.root,meta:{claudeSessionId:SESSION,expected_session_id:OTHER}})).ok).toBe(false);
});
test("a claimed session binds to its workspace and socket identity conflicts fail",async()=>{
 const f=await cleanroom();await transcript(f,SESSION,"UI Session");const channel=await sidecar(f,{CLAUDE_SESSION_ID:SESSION});try{
 expect((await f.plugin("claude",{op:"resolve_caller",cwd:f.root,endpoint_pid:process.pid,meta:{expected_session_id:SESSION}})).caller).toMatchObject({session_id:SESSION,cwd:f.root,title:"UI Session"});
 expect((await f.plugin("claude",{op:"resolve_caller",cwd:f.root,endpoint_pid:process.pid,meta:{expected_session_id:OTHER}})).ok).toBe(false);
 }finally{await channel.close();}
});
test("the wake queue drains once and survives append/drain races",async()=>{
 const f=await cleanroom();const wake=(i:number)=>f.plugin("claude",{op:"wake",session_id:SESSION,batch_id:"batch-"+i,text:"card-"+i,wake:true});
 await Promise.all(Array.from({length:30},(_,i)=>wake(i)));const hook=()=>f.command([],{executable:"plugins/claude/bin/cito-claude-hook",input:JSON.stringify({hook_event_name:"Stop",session_id:SESSION,cwd:f.root})});
 const racing=await Promise.all([hook(),...Array.from({length:30},(_,i)=>wake(i+30))]);const last=await hook();const text=String(JSON.parse(racing[0]!.stdout).reason??"")+String(JSON.parse(last.stdout).reason??"");for(let i=0;i<60;i++)expect(text.match(new RegExp(`card-${i}(?![0-9])`,"g"))).toHaveLength(1);
 expect(await queue(f)).toEqual([]);expect(JSON.parse((await hook()).stdout)).toEqual({});
});
test("badges are written beside the queue and removed with the session",async()=>{
 const f=await cleanroom();await f.plugin("claude",{op:"update_badge",session_id:SESSION,badge:"🔔 listener (1)"});const path=join(f.stateDir,"claude/badges",SESSION);expect(await readFile(path,"utf8")).toBe("🔔 listener (1)\n");await f.plugin("claude",{op:"update_badge",session_id:SESSION,badge:null});await expect(stat(path)).rejects.toMatchObject({code:"ENOENT"});
});
test("claude is registered by default and queues a card without claiming it was read",async()=>{
 const f=await cleanroom();const r=await f.plugin("claude",{op:"wake",session_id:SESSION,batch_id:"a",text:"card",wake:true});expect(r.outcome).toBe("delivered_unconfirmed");expect((await queue(f))[0]).toMatchObject({batch_id:"a",content:"card",wake:true});
});

test("a native Stop recovers abandoned staging before a persisted queue larger than 4 MiB",async()=>{
 const f=await cleanroom();for(const [id,text]of [["a","first"],["b","second"]])await f.plugin("claude",{op:"wake",session_id:SESSION,batch_id:id,text,wake:true});const path=join(f.stateDir,"claude/wakes",SESSION+".jsonl");await rename(path,path+".draining-abandoned");const large="x".repeat(Math.floor(4.2*1024*1024));await writeFile(path,JSON.stringify({batch_id:"c",content:large,wake:false,created_at_ms:3})+"\n");const result=await f.command([],{executable:"plugins/claude/bin/cito-claude-hook",input:JSON.stringify({hook_event_name:"Stop",session_id:SESSION,cwd:f.root})});expect(result.code).toBe(0);const response=JSON.parse(result.stdout);expect(response.decision).toBe("block");expect(response.reason).toBe("first\n\nsecond\n\n"+large);expect((await readdir(join(f.stateDir,"claude/wakes"))).filter(name=>name.includes(".draining"))).toEqual([]);expect(await queue(f)).toEqual([]);
});
test("native Stop merges and deduplicates fallback aliases while draining both queue files",async()=>{
 const f=await cleanroom(),alias="claude-4242";await mkdir(join(f.stateDir,"claude/aliases"),{recursive:true});await writeFile(join(f.stateDir,"claude/aliases",SESSION),alias+"\n",{mode:0o600});for(const [session_id,batch_id,text]of [[alias,"alias-1","from-fallback"],[SESSION,"alias-1","from-fallback"],[SESSION,"alias-2","from-session"]])await f.plugin("claude",{op:"wake",session_id,batch_id,text,wake:true});const result=await f.command([],{executable:"plugins/claude/bin/cito-claude-hook",input:JSON.stringify({hook_event_name:"Stop",session_id:SESSION,cwd:f.root})});expect(JSON.parse(result.stdout)).toEqual({decision:"block",reason:"from-fallback\n\nfrom-session"});expect(await queue(f)).toEqual([]);expect(await queue(f,alias)).toEqual([]);
});
