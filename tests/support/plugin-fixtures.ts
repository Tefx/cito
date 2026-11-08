import { connect } from "node:net";
import { mkdir,writeFile,readFile,utimes } from "node:fs/promises";
import { join } from "node:path";
import { NativeHarness,object,waitFor,type JsonObject } from "./native-harness.js";
export const SESSION="11111111-2222-4333-8444-555555555555",OTHER="22222222-3333-4444-8555-666666666666";
export function bridgePath(f:NativeHarness,id:string,pid=process.pid){return join(f.stateDir,"sockets",`${id}-pid-${pid}.sock`);}
export async function request(path:string,value:JsonObject,options:{timeout?:number;signal?:AbortSignal;fragment?:boolean}={}){
 return new Promise<JsonObject>((yes,no)=>{const socket=connect(path);let text="";const timer=setTimeout(()=>{socket.destroy();no(new Error("Socket deadline"));},options.timeout??2000);
 const abort=()=>{socket.destroy();no(Object.assign(new Error("Aborted"),{name:"AbortError"}));};if(options.signal?.aborted){clearTimeout(timer);abort();return;}options.signal?.addEventListener("abort",abort,{once:true});
 socket.on("connect",()=>{const bytes=Buffer.from(JSON.stringify(value)+"\n");if(options.fragment){socket.write(bytes.subarray(0,3));socket.write(bytes.subarray(3));}else socket.write(bytes);});
 socket.on("data",b=>{text+=String(b);if(text.includes("\n")){try{yes(object(JSON.parse(text.slice(0,text.indexOf("\n")))));}catch(e){no(e);}socket.destroy();}});
 socket.on("error",no);socket.on("close",()=>{clearTimeout(timer);options.signal?.removeEventListener("abort",abort);if(!text.includes("\n"))no(new Error("Socket closed before newline"));});
 });
}
export function projectDir(f:NativeHarness,cwd=f.root){return join(f.env.CITO_CLAUDE_HOME!,"projects",cwd.replace(/[^a-zA-Z0-9]/g,"-"));}
export async function transcript(f:NativeHarness,sid=SESSION,title?:string,time=Date.now()){
 const dir=projectDir(f);await mkdir(dir,{recursive:true});const file=join(dir,sid+".jsonl");await writeFile(file,title?JSON.stringify({type:"custom-title",customTitle:title})+"\n":"{}\n");await utimes(file,time/1000,time/1000);return file;
}
export async function sidecar(f:NativeHarness,env:NodeJS.ProcessEnv={},pid=process.pid){
 const child=await f.spawn("plugins/claude/bin/cito-plugin-claude",[],{env});child.child.stdin.write(JSON.stringify({op:"stdio_sidecar",endpoint_pid:pid,cwd:f.root,state_dir:f.stateDir,env:{...f.env,...env}})+"\n");
 await waitFor(()=>child.stdout.includes('"op":"ready"'),5000);return {child,path:bridgePath(f,"claude",pid),frames:()=>child.stdout.trim().split("\n").filter(Boolean).map(l=>object(JSON.parse(l))),async close(){child.child.stdin.end();await child.stop();}};
}
export async function queue(f:NativeHarness,sid=SESSION){try{return (await readFile(join(f.stateDir,"claude/wakes",sid+".jsonl"),"utf8")).trim().split("\n").filter(Boolean).map(l=>object(JSON.parse(l)));}catch(e){if((e as NodeJS.ErrnoException).code==="ENOENT")return [];throw e;}}
