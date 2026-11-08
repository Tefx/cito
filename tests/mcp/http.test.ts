import {expect,test} from "vitest";
import {readFile,writeFile,stat} from "node:fs/promises";
import {join} from "node:path";
import {DatabaseSync} from "node:sqlite";
import {transcript,SESSION} from "../support/plugin-fixtures.js";
import {agent,cleanroom,rows,provider,errorTool} from "../support/native-scenarios.js";
import {freePort,waitFor,object} from "../support/native-harness.js";

test("native HTTP binds loopback, enforces Origin, merges caller headers and rejects forged identity",async()=>{
 const {f,p}=await agent();const url=`http://127.0.0.1:${f.daemonPort}`;expect((await fetch(url+"/health")).status).toBe(200);expect((await fetch(url+"/health",{headers:{origin:"https://foreign.example"}})).status).toBe(403);
 const spoof=await f.mcp("tools/call",{name:"cito_listen",arguments:{about:"spoof"},_meta:{...p.meta,http_client_pid:1,http_transport:true}},{"x-codex-turn-metadata":JSON.stringify({thread_id:"thread"})});expect(spoof.isError).toBe(true);expect((await p.calls()).filter(c=>c.op==="resolve_caller").at(-1)?.http_client_pid).not.toBe(1);
});
test("stdio forwards through its parent Pi socket and leaves the daemon alive at EOF",async()=>{
 const f=await cleanroom();const pi=await f.piBridge();await f.daemon();const child=await f.spawn("bin/cito-mcp");child.child.stdin.write(JSON.stringify({jsonrpc:"2.0",id:1,method:"tools/call",params:{name:"cito_listen",arguments:{about:"stdio parent"}}})+"\n");await waitFor(()=>child.stdout.includes('"id":1'));const reply=JSON.parse(child.stdout.trim().split("\n")[0]!);expect(reply.result.isError).toBe(false);expect(JSON.parse(reply.result.content[0].text)).toMatchObject({harness:"pi",session_id:"pi-session-1"});child.child.stdin.end();expect((await child.result).code).toBe(0);expect((await fetch(`http://127.0.0.1:${f.daemonPort}/health`)).status).toBe(200);
});
test("native HTTP caller attribution excludes its daemon PID in favor of the client endpoint",async()=>{
 const f=await cleanroom();await f.piBridge();const daemon=await f.daemon();const result=await f.tool("cito_listen",{about:"actual client"});const row=f.query("SELECT endpoint_pid FROM local_listeners WHERE harness='pi'")[0]!;expect(row.endpoint_pid).toBe(process.pid);expect(row.endpoint_pid).not.toBe(daemon.child.pid);
});
test("singleton daemon returns zero on its port, rejects another port and releases ownership",async()=>{
 const f=await cleanroom();const daemon=await f.daemon();expect((await f.command(["daemon"])).code).toBe(0);expect((await f.command(["daemon"],{env:{CITO_DAEMON_PORT:String(await freePort())}})).code).toBe(1);await daemon.stop();const restarted=await f.daemon();expect(restarted.child.pid).not.toBe(daemon.child.pid);
});
test("native listen/unlisten CLI preserves parent bridge discovery, harness flags and error exits",async()=>{
 const f=await cleanroom();await f.piBridge();await f.daemon();const listening=await f.command(["listen","--tag","cli-tag","Working","on","V6"]);expect(listening.code).toBe(0);expect(listening.stdout).toMatch(/^Listening as alice-[a-f0-9]{16} \(workspace\): Working on V6\n$/);expect((await f.command(["listen","--team"])).stdout).toMatch(/\(workspace\)/);expect((await f.command(["unlisten","--hub","local"])).stdout).toBe("Unlistened (1 removed)\n");expect((await f.command(["unlisten"])).stdout).toBe("Unlistened (0 removed)\n");for(const args of [["listen","--scope","self","--team"],["listen","--scope","bad"],["listen","--codex","--claude"],["listen","--unknown"],["unlisten","positional"]]){const result=await f.command(args);expect(result.code).toBe(1);expect(result.stderr).toMatch(/\S/);}
});
test("version flags do not start a daemon and ensureDaemon replaces a stale owned daemon",async()=>{
 const f=await cleanroom();for(const flag of ["--version","-v"]){const r=await f.command([flag]);expect(r.stdout.trim()).toBe("cito v0.18.0");expect(r.code).toBe(0);}await expect(stat(join(f.stateDir,"state.sqlite"))).rejects.toMatchObject({code:"ENOENT"});
 const script=join(f.root,"stale.cjs"),log=join(f.root,"terminated");await writeFile(script,`const http=require('node:http'),fs=require('node:fs');const s=http.createServer((q,r)=>r.end(JSON.stringify({ok:true,pid:process.pid,stateDir:${JSON.stringify(f.stateDir)},version:'0.17.0'})));s.listen(${f.daemonPort},'127.0.0.1');process.on('SIGTERM',()=>{fs.writeFileSync(${JSON.stringify(log)},'SIGTERM');s.close(()=>process.exit(0));});`);
 const old=await f.spawn(process.execPath,[script]);await waitFor(async()=>{try{return(await fetch(`http://127.0.0.1:${f.daemonPort}/health`)).ok;}catch{return false;}});const result=await f.command(["find"]);expect(result.code).toBe(0);expect(await readFile(log,"utf8")).toBe("SIGTERM");const health=object(await(await fetch(`http://127.0.0.1:${f.daemonPort}/health`)).json());expect(health.version).toBe("0.18.0");expect(health.pid).not.toBe(old.child.pid);
});

test.each(["auto","equals","separate"])("Codex CLI %s metadata and archived unlisten remain native end-to-end",async mode=>{
 const f=await cleanroom(),id="01234567-89ab-4cde-8123-0123456789ab";const db=new DatabaseSync(join(f.env.CITO_CODEX_HOME!,"state_5.sqlite"));db.exec("CREATE TABLE threads(id TEXT PRIMARY KEY,title TEXT,cwd TEXT,git_origin_url TEXT,git_branch TEXT,archived INTEGER,name TEXT,updated_at INTEGER)");db.prepare("INSERT INTO threads VALUES(?,'Codex title',?,'https://github.com/acme/service.git','main',0,'Codex session',100)").run(id,f.root);try{await f.daemon();const flags=mode==="auto"?["--codex"]:mode==="equals"?["--codex="+id]:["--codex",id];const listen=await f.command(["listen",...flags,"--scope","workspace","Codex","review"]);expect(listen.code).toBe(0);expect(listen.stdout).toMatch(/^Listening as alice-[a-f0-9]{16} \(workspace\): Codex review\n$/);expect(f.query("SELECT session_id,harness FROM local_listeners WHERE harness='codex'")[0]).toEqual({session_id:id,harness:"codex"});db.exec("UPDATE threads SET archived=1");expect((await f.command(["unlisten","--codex"])).stdout).toBe("Unlistened (1 removed)\n");}finally{db.close();}
});
test.each(["auto","equals","separate"])("Claude CLI %s metadata resolves the real transcript and supports unlisten",async mode=>{
 const f=await cleanroom();await transcript(f,SESSION);await f.daemon();const flags=mode==="auto"?["--claude"]:mode==="equals"?["--claude="+SESSION]:["--claude",SESSION];const listen=await f.command(["listen",...flags,"Claude","task"]);expect(listen.code).toBe(0);expect(listen.stdout).toMatch(/^Listening as alice-[a-f0-9]{16} \(workspace\): Claude task\n$/);expect(f.query("SELECT session_id FROM local_listeners WHERE harness='claude'")[0]!.session_id).toBe(SESSION);expect((await f.command(["unlisten","--claude"])).stdout).toBe("Unlistened (1 removed)\n");
});
