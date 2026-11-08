import {expect,test} from "vitest";
import {mkdir,copyFile,writeFile,stat} from "node:fs/promises";
import {join} from "node:path";
import {agent,remoteAgent,cleanroom,sendUnbound} from "../support/native-scenarios.js";
import {projectRoot,waitFor,object} from "../support/native-harness.js";

test("daemon owns its lock until in-flight work and teardown finish after HTTP shutdown",async()=>{
 const {f,p,enrolled,daemon}=await remoteAgent({delay_op:"wake",delay_ms:1000});const pending=sendUnbound(f,String(enrolled.listener_id)).catch(e=>e);await waitFor(async()=>(await p.calls()).some(c=>c.op==="wake"));
 const health=object(await(await fetch(`http://127.0.0.1:${f.daemonPort}/health`)).json());process.kill(Number(health.pid),"SIGTERM");const contender=await f.command(["daemon"]);expect(contender.code).toBe(1);await pending;await daemon.result;const replacement=await f.daemon();expect(replacement.child.pid).not.toBe(health.pid);
});
test("HTTP/stdio are ready during slow UI startup and shutdown reaps that owned UI child",async()=>{
 const f=await cleanroom();const root=join(f.root,"package"),ui=join(root,"plugins/human/bin/cito-ui"),native=join(root,"bin/cito-native"),log=join(f.root,"ui-started");await mkdir(join(root,"bin"),{recursive:true});await mkdir(join(root,"plugins/human/bin"),{recursive:true});await copyFile(join(projectRoot,"bin/cito-native-darwin-arm64"),native);
 await writeFile(ui,`#!${process.execPath}\nconst fs=require('node:fs');fs.writeFileSync(${JSON.stringify(log)},String(process.pid));setTimeout(()=>console.error('slow UI ready'),6000);process.stdin.resume();process.on('SIGTERM',()=>process.exit(0));`,{mode:0o700});
 const daemon=await f.spawn(native,["daemon"],{env:{CITO_UI_BINARY:ui,CITO_DISABLE_UI:"0"}});await waitFor(async()=>{try{return(await fetch(`http://127.0.0.1:${f.daemonPort}/health`)).ok;}catch{return false;}});expect((await f.mcp("tools/list")).tools).toHaveLength(5);const forward=await f.command([],{executable:"bin/cito-mcp",input:JSON.stringify({jsonrpc:"2.0",id:1,method:"tools/list"})+"\n"});expect(forward.code).toBe(0);expect(forward.stdout).toContain("cito_listen");await waitFor(async()=>stat(log).then(()=>true,()=>false));const pid=Number(await import("node:fs/promises").then(fs=>fs.readFile(log,"utf8")));await daemon.stop();await waitFor(()=>{try{process.kill(pid,0);return false;}catch{return true;}});
},10000);
