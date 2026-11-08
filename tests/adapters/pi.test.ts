import {expect,test} from "vitest";
import {mkdir,writeFile,utimes} from "node:fs/promises";
import {cleanroom,socketServer,rows} from "../support/native-scenarios.js";
import {bridgePath,request} from "../support/plugin-fixtures.js";
import {object} from "../support/native-harness.js";

test("uses explicit pi_pid, caller_pid, or nested gateway parent_pid, and returns normalized receiver metadata",async()=>{
 const f=await cleanroom();const pi=await f.piBridge("session");for(const meta of [{pi_pid:process.pid},{caller_pid:process.pid},{gateway:{parent_pid:process.pid}}]){const r=await f.plugin("pi",{op:"resolve_caller",meta});expect(r).toMatchObject({ok:true,caller:{harness:"pi",endpoint_pid:process.pid,session_id:"session",cwd:f.root,repo:null,branch:null}});}
 expect((await f.plugin("pi",{op:"resolve_caller",meta:{pi_pid:123,caller_pid:456}})).error_code).toBe("INVALID_INPUT");await pi.stop();
});
test("walks the real ps parent chain beyond the immediate parent",async()=>{
 const f=await cleanroom();await f.piBridge("grandparent");const binary=await f.binary("plugins/pi/bin/cito-plugin-pi");const launcher="const{spawn}=require('node:child_process');const child=spawn("+JSON.stringify(binary)+",[],{env:process.env,stdio:['pipe','inherit','inherit']});process.stdin.pipe(child.stdin);child.on('close',code=>process.exit(code??1));";const result=await f.command(["-e",launcher],{executable:process.execPath,input:JSON.stringify({op:"resolve_caller",meta:{},cwd:f.root,state_dir:f.stateDir,env:f.env})+"\n"});expect(result.code).toBe(0);expect(JSON.parse(result.stdout).caller).toMatchObject({endpoint_pid:process.pid,session_id:"grandparent"});
});
test("returns BINDING_REQUIRED for absent or unopen implicit ancestors and propagates explicit PID failure",async()=>{
 const f=await cleanroom();expect((await f.plugin("pi",{op:"resolve_caller",meta:{},require_open:false})).ok).toBe(false);await mkdir(f.stateDir+"/sockets",{recursive:true});await writeFile(bridgePath(f,"pi",process.ppid),"stale");expect((await f.plugin("pi",{op:"resolve_caller",meta:{pi_pid:"123"}})).error_code).toBe("PI_BRIDGE_UNAVAILABLE");
});
test("distinguishes no active session at the bridge while explicit caller claims fail visibly",async()=>{
 const f=await cleanroom();const s=await socketServer(bridgePath(f,"pi",43210),socket=>socket.once("data",()=>socket.end('{"ok":false,"error":"SESSION_NOT_OPEN"}\n')));try{expect((await f.plugin("pi",{op:"resolve_caller",meta:{pi_pid:43210}})).error_code).toBe("PI_BRIDGE_UNAVAILABLE");}finally{await s.close();}
});
test("uses a bounded identify deadline and rejects sockets closing without newline",async()=>{
 const f=await cleanroom();const s=await socketServer(bridgePath(f,"pi",43210),socket=>socket.once("data",()=>socket.end('{"ok":true}')));try{const start=performance.now();expect((await f.plugin("pi",{op:"resolve_caller",meta:{pi_pid:43210}})).ok).toBe(false);expect(performance.now()-start).toBeLessThan(10000);}finally{await s.close();}
});
test("sends status and deliver with exact frozen batch fields and classifies session mismatch",async()=>{
 const f=await cleanroom();const seen:Record<string,unknown>[]=[];const s=await socketServer(bridgePath(f,"pi",43210),socket=>socket.once("data",buf=>{const r=object(JSON.parse(String(buf)));seen.push(r);socket.end(JSON.stringify({ok:true,session_id:"session",delivered:true})+"\n");}));
 try{expect((await f.plugin("pi",{op:"update_badge",endpoint_pid:43210,session_id:"session",badge:"badge"})).ok).toBe(true);expect((await f.plugin("pi",{op:"wake",endpoint_pid:43210,session_id:"session",batch_id:"batch",text:"card",wake:true})).outcome).toBe("delivered");expect(seen.filter(r=>r.op!=="identify")).toEqual([{op:"status",session_id:"session",badge:"badge"},{op:"deliver",session_id:"session",batch_id:"batch",content:"card",wake:true}]);}finally{await s.close();}
});
test("honors pre-aborted and mid-flight cancellation without leaving native plugin children",async()=>{
 const f=await cleanroom();const s=await socketServer(bridgePath(f,"pi",43210),()=>{});try{const child=await f.spawn("plugins/pi/bin/cito-plugin-pi");child.child.stdin.end(JSON.stringify({op:"wake",endpoint_pid:43210,session_id:"session",state_dir:f.stateDir,text:"blocked",batch_id:"cancel"})+"\n");await child.stop();expect((await child.result).signal).toBe("SIGTERM");await expect(request(bridgePath(f,"pi",43210),{op:"identify"},{signal:AbortSignal.abort()})).rejects.toMatchObject({name:"AbortError"});}finally{await s.close();}
});
test("discovers live socket sessions while isolating dead PIDs/non-sockets and fences changed sessions",async()=>{
 const f=await cleanroom();expect(rows((await f.plugin("pi",{op:"discover_sessions"})).sessions)).toEqual([]);const pi=await f.piBridge("live-current");
 expect(rows((await f.plugin("pi",{op:"discover_sessions"})).sessions)).toEqual(expect.arrayContaining([expect.objectContaining({session_id:"live-current",endpoint_pid:process.pid})]));
 await pi.switchSession("changed");expect((await f.plugin("pi",{op:"resolve_caller",meta:{pi_pid:process.pid,expected_session_id:"live-current"}})).ok).toBe(false);
});

test.each([{ok:true,session_id:"",pi_pid:43210},{ok:true,session_id:"wrong-pid",pi_pid:999}])("native identify rejects invalid identity fields before binding: %j",async response=>{
 const f=await cleanroom(),s=await socketServer(bridgePath(f,"pi",43210),socket=>socket.once("data",()=>socket.end(JSON.stringify({...response,cwd:f.root})+"\n")));try{expect((await f.plugin("pi",{op:"resolve_caller",meta:{pi_pid:43210}})).error_code).toBe("PI_BRIDGE_UNAVAILABLE");}finally{await s.close();}
});
