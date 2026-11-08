import {expect,test} from "vitest";
import {readFile,stat,writeFile,mkdir} from "node:fs/promises";
import {join} from "node:path";
import {cleanroom} from "../support/native-scenarios.js";
import {SESSION,OTHER,sidecar,request,transcript,bridgePath,queue} from "../support/plugin-fixtures.js";
import {waitFor} from "../support/native-harness.js";

test("the native bridge identifies its session, queues once per batch and refuses a stale session",async()=>{
 const f=await cleanroom(),channel=await sidecar(f,{CLAUDE_SESSION_ID:SESSION});try{
 expect(await request(channel.path,{op:"identify"})).toMatchObject({ok:true,session_id:SESSION,cwd:f.root});
 const frame={op:"deliver",session_id:SESSION,batch_id:"same",content:"card",wake:true};expect(await request(channel.path,frame)).toMatchObject({ok:true,deduplicated:false});expect(await request(channel.path,frame)).toMatchObject({deduplicated:true});expect(await request(channel.path,{...frame,session_id:OTHER})).toMatchObject({ok:false,error:"SESSION_MISMATCH"});expect(await queue(f)).toHaveLength(1);
 }finally{await channel.close();}
});
test("identify reports the title the user gave the session rather than the workspace folder",async()=>{
 const f=await cleanroom();await transcript(f,SESSION,"Human title");const c=await sidecar(f,{CLAUDE_SESSION_ID:SESSION});try{expect(await request(c.path,{op:"identify"})).toMatchObject({session_name:"Human title"});}finally{await c.close();}
});
test("explicit IDs win over scanning, transcript creation preserves the first-turn alias and clear switches sessions",async()=>{
 const f=await cleanroom();const c=await sidecar(f);try{
 const first=await request(c.path,{op:"identify"});expect(String(first.session_id)).toBe("claude-"+process.pid);await transcript(f,SESSION);const second=await request(c.path,{op:"identify"});expect(second.session_id).toBe(first.session_id);expect((await readFile(join(f.stateDir,"claude/aliases",SESSION),"utf8")).trim()).toBe(first.session_id);await transcript(f,OTHER,undefined,Date.now()+1000);expect((await request(c.path,{op:"identify"})).session_id).toBe(OTHER);
 }finally{await c.close();}
});
test("status writes badges across switches, notifications do not block delivery, and close clears badges",async()=>{
 const f=await cleanroom();await transcript(f,SESSION);const c=await sidecar(f);const path=join(f.stateDir,"claude/badges",SESSION);
 await request(c.path,{op:"status",session_id:SESSION,badge:"🔔 badge (2)"});expect(await readFile(path,"utf8")).toBe("🔔 badge (2)\n");await request(c.path,{op:"deliver",session_id:SESSION,batch_id:"native",content:"card",wake:true});expect(c.frames().some(r=>r.method==="notifications/claude/channel")).toBe(true);await c.close();await expect(stat(path)).rejects.toMatchObject({code:"ENOENT"});
});
test("only the native initialize result gains the Claude channel capability",async()=>{
 const f=await cleanroom();await f.daemon();const child=await f.spawn("bin/cito-mcp",[],{env:{CITO_CLAUDE_CHANNEL:"1",CLAUDE_SESSION_ID:SESSION}});child.child.stdin.write(JSON.stringify({jsonrpc:"2.0",id:1,method:"initialize",params:{protocolVersion:"2024-11-05",capabilities:{},clientInfo:{name:"native-test",version:"1"}}})+"\n");
 await waitFor(()=>child.stdout.split("\n").slice(0,-1).some(l=>JSON.parse(l).id===1));const frames=child.stdout.split("\n").slice(0,-1).map(l=>JSON.parse(l));expect(frames.find(r=>r.id===1).result.capabilities.experimental).toHaveProperty("claude/channel");child.child.stdin.write(JSON.stringify({jsonrpc:"2.0",id:2,method:"tools/list"})+"\n");await waitFor(()=>child.stdout.split("\n").slice(0,-1).some(l=>JSON.parse(l).id===2));expect(JSON.parse(child.stdout.split("\n").slice(0,-1).find(l=>JSON.parse(l).id===2)!).result).not.toHaveProperty("capabilities");child.child.stdin.end();await child.stop();
});
test("a second native bridge cannot steal a live endpoint but reclaims an abandoned socket",async()=>{
 const f=await cleanroom();const c=await sidecar(f,{CLAUDE_SESSION_ID:SESSION});const second=await f.command([],{executable:"plugins/claude/bin/cito-plugin-claude",input:JSON.stringify({op:"stdio_sidecar",endpoint_pid:process.pid,cwd:f.root,state_dir:f.stateDir})+"\n"});expect(second.stderr+second.stdout).toMatch(/live|owns|already/i);await c.close();
 const path=bridgePath(f,"claude");await writeFile(path,"abandoned");const recovered=await sidecar(f,{CLAUDE_SESSION_ID:SESSION});expect((await request(recovered.path,{op:"ping"})).ok).toBe(true);await recovered.close();
});
