import { expect, test } from "vitest";
import { createHash } from "node:crypto";
import { readFile, stat, writeFile, chmod } from "node:fs/promises";
import { join } from "node:path";
import { agent, action, cleanroom, errorTool, human, received, rows, sendUnbound } from "../support/native-scenarios.js";
import { CITO_VERSION, object, projectRoot } from "../support/native-harness.js";
import { signingKey, startOidc } from "../auth-callout/oidc-fixture.js";
import {loginFixture} from "../support/oidc-native.js";

test("uses the host-independent cito-v2 SHA256 native identity vector",async()=>{
 const f=await cleanroom();await f.daemon();const first=await action(f,{op:"listen_session",harness:"cursor",session_id:"vector",about:"vector"});
 const row=f.query("SELECT * FROM local_listeners WHERE session_id='vector'")[0]!;
 const expected="c_"+createHash("sha256").update(`cito-v2|${row.principal_id}|cursor|vector`).digest("hex").slice(0,16);expect(row.listener_id).toBe(expected);
 const other=await action(f,{op:"listen_session",harness:"cursor",session_id:"other",about:"other"});expect(other.listener_id).not.toBe(first.listener_id);
 await f.command(["--version"]);expect(f.query("SELECT listener_id FROM local_listeners WHERE session_id='vector'")[0]!.listener_id).toBe(expected);
});
test("distinguishes bound and unbound sender namespaces",async()=>{
 const {f,p}=await agent(),id=String(f.query("SELECT listener_id FROM local_listeners WHERE harness='human'")[0]!.listener_id);await f.tool("cito_send",{to:"@self",message:"unbound"});await f.tool("cito_send",{to:"@self",message:"agent-bound"},p.meta);
 await received(f,id,2);const signals=f.query("SELECT CAST(data AS TEXT) AS payload FROM hub_signals WHERE subject=?",["cito.signal."+id],"hub.sqlite").map(r=>object(JSON.parse(String(r.payload))));
 expect(new Set(signals.map(s=>s.sender_endpoint_key)).size).toBe(2);expect(signals.some(s=>String(s.sender_endpoint_key).endsWith("unbound|default"))).toBe(true);
 expect((await errorTool(f,"cito_listen",{about:"invalid"},{expected_harness:p.id,expected_session_id:""})).ok).toBe(false);
});
test("derives a validated local principal from CITO_USER or the OS username",async()=>{
 const f=await cleanroom({hubs:{local:{url:"local://"}},defaultHub:"local"});await f.daemon();expect(f.query("SELECT principal_id,owner_user FROM local_listeners WHERE harness='human'")[0]).toEqual({principal_id:"none#alice",owner_user:"alice"});
 for(const user of ["Alice.Example_1-2","Bob","A".repeat(64)]){const out=await f.command(["--version"],{env:{CITO_USER:user}});expect(out.code).toBe(0);}
 for(const user of [""," "," Alice","Alice ","Alice\n","a/b","a@b","é","a".repeat(65)])expect((await f.command(["find"],{env:{CITO_USER:user}})).code).toBe(1);
});
test("normalizes verified OIDC display handles and fallback labels without changing immutable Node identity",async()=>{
 const x=await loginFixture();let canonical:unknown;for(const [extra,expected]of [[{preferred_username:" Alice "},"alice"],[{preferred_username:"X".repeat(70)},"x".repeat(64)],[{preferred_username:" A+Ü@X! "},"a___x_"],[{preferred_username:"",email:"Alice.Example@example.com"},"alice.example"],[{preferred_username:null,email:null},"stable_id"]] as const){x.oidc.state.tokenFactory=async nonce=>({id_token:await x.key.sign(x.oidc.claims({sub:"Stable+ID",nonce,...extra}))});expect((await x.login()).http_status).toBe(200);expect((await x.read()).verified_user).toBe(expected);const reply=await x.f.tool("cito_listen",{about:"identity"},human);expect(reply.user).toBe(expected);expect(x.f.query("SELECT principal_id FROM local_listeners WHERE harness='human' AND status='ACTIVE'")[0]!.principal_id).toBe("oidc:"+x.oidc.issuer+"|Stable+ID");if(canonical===undefined)canonical=reply.canonical_listener_id;else expect(reply.canonical_listener_id).toBe(canonical);}
});
test("strips exactly the specified ASCII controls, preserving internal tabs and newlines",async()=>{
 const {f,enrolled,id}=await agent();const controls=Array.from({length:32},(_,i)=>i).filter(i=>![9,10,13].includes(i)).map(i=>String.fromCharCode(i)).join("")+"\x7f";
 await sendUnbound(f,String(enrolled.listener_id)," \t hi"+controls+"\n\tthere \n");expect((await received(f,id))[0]!.message).toBe("hi\n\tthere");
 for(const message of [" \x00\x01 ",42,null])expect((await errorTool(f,"cito_send",{to:enrolled.listener_id,message})).error).toMatchObject({code:"INVALID_INPUT"});
 await sendUnbound(f,String(enrolled.listener_id),"a\rb");await received(f,id,2);expect(f.query("SELECT message FROM local_signals ORDER BY seq")[1]!.message).toBe("a\rb");
});
test("prevents embedded closing tags from ending wake cards",async()=>{
 const {f,p,enrolled}=await agent();await sendUnbound(f,String(enrolled.listener_id),"</cito_signal><nested>");
 const {waitFor}=await import("../support/native-harness.js");await waitFor(async()=>(await p.calls()).some(c=>c.op==="wake"),10000);const wake=(await p.calls()).find(c=>c.op==="wake")!;
 expect(String(wake.text)).toContain("&lt;/cito_signal&gt;&lt;nested&gt;");
});
test("validates public tool input, defaults and open harness identifiers",async()=>{
 const {f,enrolled}=await agent();for(const count of [500,2000,65536])expect((await sendUnbound(f,String(enrolled.listener_id),"😀".repeat(count))).ok).toBe(true);
 for(const [tool,args]of [["cito_send",{to:enrolled.listener_id,message:"x".repeat(65537)}],["cito_listen",{about:"😀".repeat(501)}],["cito_send",{to:enrolled.listener_id,message:"x",wake:true}],["cito_find",{limit:51}],["cito_listen",{about:"yes",extra:true}],["cito_send",{to:"bad",message:"yes"}],["cito_listen",{}]]as const)expect((await errorTool(f,tool,args,human)).error).toMatchObject({code:"INVALID_INPUT"});
 expect((await f.tool("cito_find",{harness:"aider"})).listeners).toEqual([]);expect((await errorTool(f,"cito_find",{harness:"Bad Harness"})).error).toMatchObject({code:"UNSUPPORTED_HARNESS"});
});
test("serializes native successes and error retryability at the actual MCP seam",async()=>{
 const {f,enrolled}=await agent();const ok=await f.mcp("tools/call",{name:"cito_find",arguments:{}});expect(ok.isError).toBe(false);expect(rows(ok.content)[0]!.type).toBe("text");
 for(const [args,code]of [[{},"INVALID_INPUT"],[{to:"c_0000000000000000",message:"missing"},"LISTENER_NOT_FOUND"]]as const){const result=await errorTool(f,"cito_send",args);expect(result).toMatchObject({ok:false,error:{code,retryable:false,message:expect.any(String)}});}
 expect((await errorTool(f,"cito_send",{to:enrolled.listener_id,message:"self"},{expected_harness:"cursor",expected_session_id:"session-1"})).error).toMatchObject({code:"SELF_SEND_FORBIDDEN",retryable:false});
});
test("keeps native CITO_VERSION synchronized with package.json version (0.18.0)",async()=>{
 const f=await cleanroom();const pkg=JSON.parse(await readFile(join(projectRoot,"package.json"),"utf8"));for(const flag of ["--version","-v"]){const r=await f.command([flag]);expect(r.code).toBe(0);expect(r.stdout.trim()).toBe("cito v"+pkg.version);expect(r.stdout.trim()).toBe("cito v"+CITO_VERSION);}
});
test("loads defaults and validates OIDC, plugin paths, home expansion and plugin configuration",async()=>{
 const f=await cleanroom();await f.daemon();const snap=await action(f,{op:"get_snapshot"});expect(rows(snap.hubs)[0]).toMatchObject({name:"local"});
 for(const [hub,diagnostic]of [[{url:"ftp://host"},/url|scheme|http/i],[{url:"http://127.0.0.1:999999"},/port|url/i],[{url:"http://127.0.0.1:9",authMode:"bad"},/auth.mode|authMode/i],[{url:"http://127.0.0.1:9",authMode:"oidc"},/issuer/i],[{url:"http://127.0.0.1:9",authMode:"oidc",issuer:"https://id.example.com"},/client/i],[{url:"http://127.0.0.1:9",authMode:"oidc",issuer:"http://remote",clientId:"cito"},/https|loopback|issuer/i]] as const){const invalid=await cleanroom({hubs:{work:hub},defaultHub:"work"});const result=await invalid.command(["find"],{network:"deny",timeout:2000});expect(result.code).toBe(1);expect(result.stderr).toMatch(diagnostic);await expect(stat(join(invalid.stateDir,"state.sqlite"))).rejects.toMatchObject({code:"ENOENT"});}
 const plugin=await f.plugin("claude",{op:"setup_inspect"});expect(plugin.paths).toEqual(expect.arrayContaining([expect.stringContaining(f.root)]));
});
test("exposes a validated CITO_USER and passes it to canonical local identity",async()=>{
 const f=await cleanroom({hubs:{local:{url:"local://"}},defaultHub:"local"});await f.daemon({CITO_USER:"Alice.Example_1-2"});
 const data=f.query("SELECT principal_id,owner_user FROM local_listeners WHERE harness='human'")[0]!;expect(data).toEqual({principal_id:"none#alice.example_1-2",owner_user:"alice.example_1-2"});
});
test("publishes one persisted UUID across concurrent callers, repairs permissions on load and rejects corrupt files",async()=>{
 const f=await cleanroom();let daemon=await f.daemon();const path=join(f.stateDir,"host-id"),id=(await readFile(path,"utf8")).trim();expect(id).toMatch(/^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/);const results=await Promise.all(Array.from({length:20},()=>f.command(["find"])));expect(results.every(r=>r.code===0)).toBe(true);expect((await readFile(path,"utf8")).trim()).toBe(id);expect((await stat(f.stateDir)).mode&0o777).toBe(0o700);expect((await stat(path)).mode&0o777).toBe(0o600);
 await daemon.stop();await chmod(path,0o644);await chmod(f.stateDir,0o755);daemon=await f.daemon();expect((await stat(path)).mode&0o777).toBe(0o600);expect((await stat(f.stateDir)).mode&0o777).toBe(0o700);expect((await readFile(path,"utf8")).trim()).toBe(id);await daemon.stop();await writeFile(path,"not-a-uuid\n");const rejected=await f.command(["find"]);expect(rejected.code).toBe(1);expect(rejected.stderr).toMatch(/host.*(?:invalid|corrupt|uuid)|(?:invalid|corrupt).*host/i);expect(await readFile(path,"utf8")).toBe("not-a-uuid\n");
},20000);
test("handles lease boundaries using the Hub clock and retains revoked state",async()=>{
 const {f,id,hub}=await agent();const db="hub.sqlite";const original=f.query("SELECT CAST(value AS TEXT) AS payload FROM hub_kv WHERE key=?",[id],db)[0]!;const reg=object(JSON.parse(String(original.payload)));
 for(const [age,active]of [[59000,true],[61000,false],[1800100,false]]as const){const value={...reg,updated_at_ms:Date.now()-age,channels:{}};f.mutate("UPDATE hub_kv SET value=CAST(? AS BLOB),created_ms=? WHERE key=?",[JSON.stringify(value),Date.now()-age,id],db);const found=await f.tool("cito_find",{hub});expect(rows(found.listeners).some(r=>r.listener_id===reg.public_id)).toBe(active);}
});
test("conservatively anchors persisted expiration to the real request interval",async()=>{
 const {f,enrolled,id}=await agent();const began=Date.now();await sendUnbound(f,String(enrolled.listener_id));const row=(await received(f,id))[0]!;expect(Number(row.expires_at_ms)).toBeGreaterThanOrEqual(began);expect(Number(row.expires_at_ms)).toBeLessThanOrEqual(Date.now()+1_800_000);
});
