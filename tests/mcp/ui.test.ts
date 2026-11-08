import {expect,test} from "vitest";
import {readFile,writeFile} from "node:fs/promises";
import {join} from "node:path";
import {createHash} from "node:crypto";
import {agent,remoteAgent,cleanroom,provider,seed,sendUnbound,received,action,rows,errorTool,human} from "../support/native-scenarios.js";
import {freePort,waitFor,object} from "../support/native-harness.js";
import {loginFixture} from "../support/oidc-native.js";

test("hub invites reject non-object JSON without crashing or changing config", async () => {
 const f = await cleanroom();
 const daemon = await f.daemon();
 const before = await readFile(f.configPath, "utf8");
 for (const invite of ["[]", "42", "true", '"invite"', "null"]) {
  expect(await action(f, {op:"paste_hub_invite",invite}, true)).toMatchObject({
   http_status:400,ok:false,error:"INVALID_INPUT: Invalid hub invite"
  });
  expect(await readFile(f.configPath, "utf8")).toBe(before);
  expect((await action(f, {op:"get_snapshot"})).http_status).toBe(200);
 }
 expect(daemon.child.exitCode).toBeNull();
 expect(await action(f, {op:"paste_hub_invite",invite:JSON.stringify({name:"work",url:"local://",authMode:"none"})})).toMatchObject({http_status:200});
 expect(JSON.parse(await readFile(f.configPath, "utf8")).hubs.work).toMatchObject({url:"local://",authMode:"none"});
});
test("UI and MCP boundaries handle non-object JSON and normalize malformed tool metadata", async () => {
 const f = await cleanroom();
 const daemon = await f.daemon();
 for (const value of [[], 42, true, "request", null]) {
  for (const path of ["/ui", "/action", "/control", "/mcp"]) {
   const response = await fetch(`http://127.0.0.1:${f.daemonPort}${path}`, {
    method:"POST",headers:{"content-type":"application/json"},body:JSON.stringify(value),signal:AbortSignal.timeout(3000)
   });
   expect(response.status).toBe(400);
   const body = object(await response.json());
   if (path === "/mcp") expect(body.error).toMatchObject({code:-32600,message:"Invalid Request"});
   else expect(body.error).toBe("INVALID_INPUT: Action must be an object");
  }
  const response = await fetch(`http://127.0.0.1:${f.daemonPort}/mcp`, {
   method:"POST",headers:{"content-type":"application/json"},
   body:JSON.stringify({jsonrpc:"2.0",id:1,method:"tools/call",params:value}),signal:AbortSignal.timeout(3000)
  });
  expect(response.status).toBe(200);
  expect(object(await response.json()).result).toMatchObject({isError:true});
  expect(await f.mcp("tools/call", {name:"cito_find",arguments:{},_meta:value})).toMatchObject({isError:false});
 }
 expect(await f.mcp("ping")).toEqual({});
 expect(daemon.child.exitCode).toBeNull();
});
test("host and listener mutations reject non-object values without overwriting them", async () => {
 const owner = await cleanroom(), hub = await owner.hub();
 const f = await cleanroom({hubs:{work:{url:hub.url}},defaultHub:"work"});
 await f.daemon();
 const config = await readFile(f.configPath,"utf8"), id = "c_1111111111111111";
 for (const value of [[], 42, true, "invalid"]) {
  for (const key of ["host_hub", "hostHub"]) {
   const raw = JSON.parse(config); raw[key] = value;
   await writeFile(f.configPath, JSON.stringify(raw));
   const before = await readFile(f.configPath,"utf8");
   expect(await action(f, {op:"set_host_hub",enabled:false}, true)).toMatchObject({
    http_status:400,error:"INVALID_INPUT: Host hub must be an object"
   });
   expect(await readFile(f.configPath,"utf8")).toBe(before);
  }
  await writeFile(f.configPath, config);
  const data = Buffer.from(JSON.stringify(value)).toString("base64");
  await hub.rpc("kv_put", {key:id,data_b64:data});
  expect(await action(f, {op:"set_scope",hub:"work",listener_id:id,scope:"self"}, true)).toMatchObject({
   http_status:400,error:"INVALID_INPUT: Listener must be an object"
  });
  expect(object(await hub.rpc("kv_get",{key:id})).value_b64).toBe(data);
 }
 // Missing maps retain the existing local-hub auto-vivification behavior.
 await f.writeConfig({});
 expect((await action(f,{op:"setup_toggle_hub",hub:"local",enabled:false})).http_status).toBe(200);
 expect(JSON.parse(await readFile(f.configPath,"utf8")).hubs.local.enabled).toBe(false);
 expect((await action(f,{op:"hub_action",hub:"local",action:"reconnect"})).http_status).toBe(200);
 expect(JSON.parse(await readFile(f.configPath,"utf8")).hubs.local.enabled).toBe(true);
});

test.each([true,false])("cito_send desktop banner admission respects desktopBanner=%s",async desktopBanner=>{
 const f=await cleanroom({desktopBanner}),p=await provider(f);
 const log=join(f.root,"ui-frames.jsonl"),ui=join(f.root,"ui-reader.mjs");
 await writeFile(ui,`#!${process.execPath}\nimport fs from "node:fs";import readline from "node:readline";for await(const line of readline.createInterface({input:process.stdin})){fs.appendFileSync(${JSON.stringify(log)},line+"\\n");}`,{mode:0o700});
 await f.daemon({CITO_DISABLE_UI:"0",CITO_UI_BINARY:ui});
 const target=await f.tool("cito_listen",{about:"Banner target",scope:"workspace"},p.meta);
 const sent=await sendUnbound(f,String(target.listener_id),"Admission banner");
 const frames=async()=>{try{return(await readFile(log,"utf8")).trim().split("\n").filter(Boolean).map(l=>object(JSON.parse(l)));}catch(error){if((error as NodeJS.ErrnoException).code==="ENOENT")return[];throw error;}};
 await received(f,String(f.query("SELECT listener_id FROM local_listeners WHERE harness='cursor'")[0]!.listener_id));
 const seq=Number(f.query("SELECT seq FROM local_signals WHERE signal_id=?",[String(sent.signal_id)])[0]!.seq);
 await waitFor(()=>f.query("SELECT 1 FROM hub_consumer_acks WHERE seq=?",[seq],"hub.sqlite").length>0);
 await action(f,{op:"open_settings"});
 await waitFor(async()=>(await frames()).some(frame=>frame.op==="settings"));
 const banners=(await frames()).filter(frame=>frame.op==="banner");
 expect(banners).toHaveLength(desktopBanner?1:0);
 if(desktopBanner)expect(banners[0]!.payload).toMatchObject({message:"Admission banner",signal_id:sent.signal_id,target_harness:"cursor",target_session_id:"session-1",from_user:"alice",hub:"local"});
});
test("UI send_signal validates tool arguments before persisting and reports composer results", async () => {
 const owner = await cleanroom(), hub = await owner.hub();
 const f = await cleanroom({hubs:{work:{url:hub.url,user:"alice"}},defaultHub:"work"});
 const log = join(f.root,"composer-frames.jsonl"), ui = join(f.root,"ui-reader.mjs");
 await writeFile(ui, `#!${process.execPath}\nimport fs from "node:fs";import readline from "node:readline";for await(const line of readline.createInterface({input:process.stdin})){const frame=JSON.parse(line);if(frame.op==="composer_result")fs.appendFileSync(${JSON.stringify(log)},line+"\\n");}`, {mode:0o700});
 await f.daemon({CITO_DISABLE_UI:"0",CITO_UI_BINARY:ui});
 const id = "c_1111111111111111";
 await hub.rpc("kv_put", {key:id,data_b64:Buffer.from(JSON.stringify({
  listener_id:id,public_id:"bob-1111111111111111",owner_user:"bob",principal_id:"none#bob",
  harness:"human",session_id:"default",scope:"workspace",state:"ACTIVE",binding_generation:1,
  channels:{},title:"Human target",about:"Human target",updated_at_ms:Date.now()
 })).toString("base64")});
 const invalid = [
  {to:"@bob/"}, {in_reply_to:42}, {in_reply_to:""}, {idempotency_key:""},
  {hub:42}, {hub:"BAD!"}, {extra:"unknown"}
 ];
 for (const args of invalid) {
  expect(await action(f,{op:"send_signal",to:"@bob",message:"hello",...args},true)).toMatchObject({
   http_status:400,error:expect.stringContaining("INVALID_INPUT:")
  });
 }
 expect(f.query("SELECT * FROM local_sent_idempotency")).toEqual([]);
 expect(owner.query("SELECT * FROM hub_signals",[],"hub/hub.sqlite")).toEqual([]);
 const frames = async () => {
  try { return (await readFile(log,"utf8")).trim().split("\n").filter(Boolean).map(line=>object(JSON.parse(line))); }
  catch (error) { if ((error as NodeJS.ErrnoException).code === "ENOENT") return []; throw error; }
 };
 await waitFor(async () => (await frames()).length === invalid.length);
 expect(await frames()).toEqual(invalid.map(()=>({op:"composer_result",ok:false,error:expect.stringContaining("INVALID_INPUT:")})));
 const sent = await action(f,{op:"send_signal",to:"@bob",hub:"work",message:"hello",in_reply_to:"original",idempotency_key:"valid"});
 expect(sent).toMatchObject({http_status:200,to:"work:bob-1111111111111111"});
 expect(f.query("SELECT signal_id,status FROM local_sent_idempotency")).toEqual([{signal_id:String(sent.signal_id).split(":").at(-1),status:"wake_accepted"}]);
 const stored = owner.query("SELECT CAST(data AS TEXT) AS payload FROM hub_signals",[],"hub/hub.sqlite");
 expect(stored).toHaveLength(1);
 expect(object(JSON.parse(String(stored[0]!.payload)))).toMatchObject({to:id,message:"hello",in_reply_to:"original"});
 await waitFor(async () => (await frames()).length === invalid.length + 1);
 expect((await frames()).at(-1)).toEqual({op:"composer_result",ok:true,error:null});
});
test("send rejects --wake before starting a daemon",async()=>{
 const f=await cleanroom();const result=await f.command(["send","--to","@self","--message","hello","--wake"]);
 expect(result.code).toBe(1);expect(result.stderr).toContain("--wake is not supported");
 await expect(fetch(`http://127.0.0.1:${f.daemonPort}/health`)).rejects.toThrow();
});
test("disconnect retains ACTIVE rows, suppresses network traffic and reconnect restores receivers",async()=>{
 const {f,p,id,proxy,enrolled}=await remoteAgent();proxy.hold("consumer_backlog");const before=proxy.calls.filter(c=>c.op==="consumer_backlog").length;await waitFor(()=>proxy.calls.filter(c=>c.op==="consumer_backlog").length>before);await action(f,{op:"hub_action",hub:"work",action:"disconnect"});proxy.release("consumer_backlog");const count=proxy.calls.length;await new Promise(r=>setTimeout(r,500));expect(proxy.calls.length).toBe(count);expect(f.query("SELECT status FROM local_listeners WHERE listener_id=?",[id])[0]!.status).toBe("ACTIVE");await action(f,{op:"hub_action",hub:"work",action:"reconnect"});await sendUnbound(f,String(enrolled.listener_id));await received(f,id);
});
test("KV-first scope changes preserve ownership and reconcile authoritative remote scope",async()=>{
 const {f,p,id,row,hub,proxy}=await remoteAgent();proxy.fail("kv_update",1);expect((await action(f,{op:"set_scope",hub:"work",listener_id:id,scope:"self"},true)).http_status).toBe(400);expect(f.query("SELECT scope FROM local_listeners WHERE listener_id=?",[id])[0]!.scope).toBe("workspace");proxy.restore();await action(f,{op:"set_scope",hub:"work",listener_id:id,scope:"self"});expect(f.query("SELECT scope,owner_token,binding_generation FROM local_listeners WHERE listener_id=?",[id])[0]).toEqual({scope:"self",owner_token:row.owner_token,binding_generation:row.binding_generation});
 const remote=object(await hub.rpc("kv_get",{key:id})),reg=object(JSON.parse(Buffer.from(String(remote.value_b64),"base64").toString()));await hub.rpc("kv_update",{key:id,revision:remote.revision,data_b64:Buffer.from(JSON.stringify({...reg,scope:"workspace"})).toString("base64")});await waitFor(()=>f.query("SELECT scope FROM local_listeners WHERE listener_id=?",[id])[0]!.scope==="workspace");
});
test("UI snapshots obey ownership fences and unlisten/all make pending signals passive before quit",async()=>{
 const {f,p,row,id}=await agent();seed(f,row,"pending");await action(f,{op:"unlisten",hub:"local",listener_id:id});expect(f.query("SELECT state FROM local_signals WHERE signal_id='pending'")[0]!.state).toBe("STORED_PASSIVE");expect(rows((await action(f,{op:"get_snapshot"})).listeners).some(r=>r.listener_id===id)).toBe(false);await action(f,{op:"unlisten_all"});expect(f.query("SELECT * FROM local_listeners WHERE status='ACTIVE'")).toEqual([]);await action(f,{op:"quit"});await waitFor(async()=>{try{await fetch(`http://127.0.0.1:${f.daemonPort}/health`);return false;}catch{return true;}});
});
test("disconnect aborts interactive login and fences late connection completion",async()=>{
 const x=await loginFixture();x.oidc.state.holdAuthorization=true;
 const login=action(x.f,{op:"login_hub",hub:"work"},true,3000);await waitFor(()=>x.oidc.state.authorizationRequests===1);
 expect((await action(x.f,{op:"get_snapshot"},false,1500)).http_status).toBe(200);
 expect((await action(x.f,{op:"hub_action",hub:"work",action:"disconnect"},false,1500)).http_status).toBe(200);
 const result=await login;
 expect(result).toMatchObject({http_status:400,error:expect.stringContaining("cancelled")});
 expect(x.oidc.state.codeRequests).toBe(0);
 expect(rows((await action(x.f,{op:"get_snapshot"})).hubs).find(h=>h.name==="work")?.status).not.toBe("connected");
});
test("hub scopes and redirect ports cannot leak from the selected default hub",async()=>{
 const a=await freePort(),b=await freePort(),f=await cleanroom({hubs:{personal:{url:"local://",scopes:["openid","email"],redirectPort:a},work:{url:"local://",scopes:["openid","profile"],redirectPort:b}},defaultHub:"personal"});await f.daemon();for(const [hub,port,scopes]of [["personal",a,["openid","email"]],["work",b,["openid","profile"]]]as const){const invite=await action(f,{op:"generate_hub_invite",hub});expect(invite.invite).toMatchObject({redirectPort:port,scopes:[...scopes]});}
});
test("noninteractive startup never opens a browser and changing principal fences old listeners",async()=>{
 const x=await loginFixture();expect(x.oidc.state.authorizationRequests).toBe(0);expect((await x.login()).http_status).toBe(200);const p=await provider(x.f);await p.set({session_id:"old"});await action(x.f,{op:"listen_session",hub:"work",harness:"cursor",session_id:"old"});await x.cache(await x.key.sign(x.oidc.claims()));x.oidc.state.tokenFactory=async()=>({id_token:await x.key.sign(x.oidc.claims({sub:"new-principal"}))});await x.login();expect(x.f.query("SELECT status FROM local_listeners WHERE session_id='old'")[0]!.status).toBe("FENCED_INACTIVE");
});
test("available-session refresh filters active listeners synchronously and fences changed sessions",async()=>{
 const f=await cleanroom();const p=await provider(f);await f.daemon();await waitFor(async()=>rows((await action(f,{op:"get_snapshot"})).available).some(s=>s.harness==="cursor"));await action(f,{op:"listen_session",harness:"cursor",session_id:"session-1",cwd:f.root,about:"UI"});expect(rows((await action(f,{op:"get_snapshot"})).available).some(s=>s.session_id==="session-1")).toBe(false);await p.set({session_id:"changed"});const calls=(await p.calls()).filter(c=>c.op==="discover_sessions").length;await action(f,{op:"refresh_available"});await waitFor(async()=>(await p.calls()).filter(c=>c.op==="discover_sessions").length>calls);expect((await action(f,{op:"listen_session",harness:"cursor",session_id:"session-1",cwd:f.root},true)).http_status).toBe(400);
});
test("directory queries expose remote peers and apply prefix tokens and facets",async()=>{
 const {f,hub}=await remoteAgent();for(const [id,title,harness]of [["c_1111111111111111","Build API","cursor"],["c_2222222222222222","Review UI","aider"]])await hub.rpc("kv_put",{key:id,data_b64:Buffer.from(JSON.stringify({listener_id:id,public_id:title!.toLowerCase().replaceAll(" ","-")+"-"+id!.slice(2),state:"ACTIVE",scope:"workspace",owner_user:"alice",principal_id:"none#alice",host_id:"remote",host_name:"laptop",harness,title,about:title,repo:"acme/api",channels:{remote:{state:"ACTIVE"}}})).toString("base64")});const all=await action(f,{op:"query_directory",hub:"work",query:""});expect(rows(all.peers).length).toBeGreaterThanOrEqual(2);const selected=rows((await action(f,{op:"query_directory",hub:"work",query:"harness:cursor build"})).peers);expect(selected).toHaveLength(1);expect(selected[0]!.title).toBe("Build API");expect(rows((await action(f,{op:"get_snapshot"})).my_remote_listeners).length).toBeGreaterThanOrEqual(2);
});
test("directory queries and snapshots normalize retained OIDC identity and share token semantics", async () => {
 const owner = await cleanroom(), hub = await owner.hub();
 // A closed loopback issuer keeps retained-identity projection independent of login and external services.
 const issuer = `https://127.0.0.1:${await freePort()}`, principal = `oidc:${issuer}|sub-1`;
 const f = await cleanroom({hubs:{work:{url:hub.url,authMode:"oidc",issuer,clientId:"cito"}},defaultHub:"work"});
 const token = "retained-identity";
 await writeFile(join(f.stateDir,"oidc-token-work.json"), JSON.stringify({
  issuer,client_id:"cito",id_token:token,token_sha256:createHash("sha256").update(token).digest("hex"),
  expires_at:0,verified_principal_id:principal,verified_user:"bob"
 }), {mode:0o600});
 await f.daemon();
 const id = "c_1111111111111111", publicId = "billing-1111111111111111", otherId = "ledger-2222222222222222";
 const target = {listener_id:id,public_id:publicId,state:"ACTIVE",scope:"self",principal_id:`${issuer}#sub-1`,
  owner_user:"bob",harness:"cursor",title:"Title-marker",about:"About-marker",repo:"acme/billing",
  cwd:"/projects/payments",branch:"feature/invoices",host_name:"desktop",host_id:"host-remote",channels:{remote:{state:"ACTIVE"}}};
 for (const peer of [
  target,
  {...target,listener_id:"c_2222222222222222",public_id:otherId,principal_id:principal,harness:"cursor-preview",repo:"acme/ledger",title:"Ledger",about:"Ledger",cwd:"/projects/ledger",branch:"main",host_name:"laptop",host_id:"host-other"},
  {...target,listener_id:"c_3333333333333333",public_id:"private-3333333333333333",principal_id:`${issuer}#someone-else`},
  {...target,listener_id:"c_4444444444444444",public_id:"shared-4444444444444444",principal_id:`${issuer}#someone-else`,scope:"workspace",owner_user:"alice",harness:"aider",repo:"acme/other",title:"Other",about:"Other",cwd:"/projects/other",branch:"main",host_name:"other",host_id:"host-other"}
 ]) {
  await hub.rpc("kv_put", {key:peer.listener_id,data_b64:Buffer.from(JSON.stringify(peer)).toString("base64")});
 }
 const queries: [string, string[]][] = [
  ["@bob",[publicId,otherId]], ["user:bob",[publicId,otherId]],
  ["is:cursor",[publicId]], ["is:cur",[]], ["harness:cur",[publicId,otherId]],
  [`#${publicId}`,[publicId]], [`#work:${publicId}`,[publicId]], [`#${id}`,[publicId]],
  ["repo:billing",[publicId]], ["host:desktop",[publicId]], ["host:host-remote",[publicId]],
  ["@BOB is:cursor repo:billing",[publicId]],
  ...["title-marker","about-marker","/projects/payments","billing","feature/invoices","desktop","host-remote",`work:${publicId}`,id].map(query => [query,[publicId]] as [string,string[]]),
  ["bob",[publicId,otherId]], ["cursor",[publicId,otherId]],
  ["@",[]], ["#",[]], ["is:",[]], ["repo:",[]], ["host:",[]], ["user:",[]]
 ];
 for (const [query, expected] of queries) {
  const peers = rows((await action(f,{op:"query_directory",hub:"work",query})).peers);
  const listeners = rows((await f.tool("cito_find",{hub:"work",query},human)).listeners);
  for (const found of [peers,listeners]) {
   expect(found.map(peer => peer.public_id).sort(), query).toEqual([...expected].sort());
   expect(found.every(peer => peer.is_self_principal === true), query).toBe(true);
  }
 }
 const mine = rows((await action(f,{op:"query_directory",hub:"work",scope:"mine"})).peers);
 expect(mine.map(peer => peer.public_id).sort()).toEqual([publicId,otherId].sort());
 const shared = rows((await f.tool("cito_find",{hub:"work",query:"@alice"},human)).listeners);
 expect(shared).toHaveLength(1);
 expect(shared[0]).toMatchObject({public_id:"shared-4444444444444444",is_self_principal:false});
 const snapshot = rows((await action(f,{op:"get_snapshot"})).my_remote_listeners);
 expect(snapshot.map(peer => peer.public_id).sort()).toEqual([publicId,otherId].sort());
 expect(snapshot.every(peer => peer.is_self_principal === true)).toBe(true);
}, 15_000);
test("directory facets filter exact harness and ownership before the result limit",async()=>{
 const {f,hub,row}=await remoteAgent();
 const own=String(row.principal_id),target="z-match-ffffffffffffffff",sameHarness="y-other-eeeeeeeeeeeeeeee",otherHarness="y-other-dddddddddddddddd";
 for(let i=0;i<55;i++){
  const hex=(i+1).toString(16).padStart(16,"0"),id="c_"+hex;
  const principal=i<52?"none#bob":own,harness=i===52?"cursor-preview":i===53?"aider":"cursor";
  const public_id=i<52?"a-decoy-"+hex:i===52?sameHarness:i===53?otherHarness:target;
  await hub.rpc("kv_put",{key:id,data_b64:Buffer.from(JSON.stringify({listener_id:id,public_id,state:"ACTIVE",scope:"workspace",principal_id:principal,owner_user:i<52?"bob":"alice",host_id:"remote",host_name:"laptop",harness,title:"Facet fixture",about:"Facet fixture",channels:{remote:{state:"ACTIVE"}}})).toString("base64")});
 }
 const query={hub:"work",query:"Facet"};
 const all=rows((await action(f,{op:"query_directory",...query,scope:"all",harness:"all"})).peers);
 expect(all).toHaveLength(50);
 expect(all.some(peer=>peer.public_id===target)).toBe(false);
 for(const op of ["query_directory","open_directory"]){
  for(const scope of ["mine","self"]){
   const peers=rows((await action(f,{op,...query,scope,harness:"cursor"})).peers);
   expect(peers.map(peer=>peer.public_id)).toEqual([target]);
   expect(peers[0]).toMatchObject({harness:"cursor",is_self_principal:true});
  }
 }
 const mine=rows((await action(f,{op:"query_directory",...query,scope:"mine",harness:"all"})).peers);
 expect(mine.map(peer=>peer.public_id).sort()).toEqual([target,sameHarness,otherHarness].sort());
 const other=rows((await action(f,{op:"query_directory",...query,scope:"all",harness:"aider"})).peers);
 expect(other.map(peer=>peer.public_id)).toEqual([otherHarness]);
});
test("directory snapshots retain concurrent upserts/removals and omit revoked KV entries",async()=>{
 const {f,hub,proxy,id}=await remoteAgent();proxy.hold("kv_snapshot");const pending=action(f,{op:"query_directory",hub:"work"});await waitFor(()=>proxy.calls.some(c=>c.op==="kv_snapshot"));const entry=object(await hub.rpc("kv_get",{key:id}));const reg=object(JSON.parse(Buffer.from(String(entry.value_b64),"base64").toString()));await hub.rpc("kv_update",{key:id,revision:entry.revision,data_b64:Buffer.from(JSON.stringify({...reg,state:"UNLISTENED"})).toString("base64")});proxy.release("kv_snapshot");await pending;expect(rows((await action(f,{op:"query_directory",hub:"work"})).peers).some(r=>r.listener_id===id||r.listener_id===reg.public_id)).toBe(false);
});
test("multi-hub candidate retention and atomic switch migration preserve the source on failure",async()=>{
 const {f,p,id}=await agent({hubs:{personal:{url:"local://"},work:{url:"local://"}},defaultHub:"personal"});const before=f.query("SELECT status FROM local_listeners WHERE hub='personal' AND listener_id=?",[id])[0];expect((await action(f,{op:"switch_hub",from_hub:"personal",to_hub:"missing",listener_id:id},true)).http_status).toBe(400);expect(f.query("SELECT status FROM local_listeners WHERE hub='personal' AND listener_id=?",[id])[0]).toEqual(before);await action(f,{op:"switch_hub",from_hub:"personal",to_hub:"work",listener_id:id});expect(f.query("SELECT status FROM local_listeners WHERE hub='work' AND listener_id=?",[id])[0]!.status).toBe("ACTIVE");expect(f.query("SELECT status FROM local_listeners WHERE hub='personal' AND listener_id=?",[id])[0]!.status).toBe("UNLISTENED");
});
test("host sharing reuses the daemon's local Hub and setup hub mutations hot-reload live slots",async()=>{
 const f=await cleanroom();await f.daemon();const port=await freePort();await action(f,{op:"set_host_hub",enabled:true,port,bind:"127.0.0.1",auth_mode:"none"});const remote=await fetch(`http://127.0.0.1:${port}/v1/rpc`,{method:"POST",headers:{"content-type":"application/json"},body:JSON.stringify({op:"kv_snapshot"})});const payload=object(await remote.json());expect(rows(object(payload.result).entries).map(entry=>object(JSON.parse(Buffer.from(String(entry.value_b64),"base64").toString()))).some(node=>node.harness==="human")).toBe(true);await action(f,{op:"set_host_hub",enabled:false});await action(f,{op:"setup_save_hub",name:"work",url:"local://",auth_mode:"none"});expect(rows((await action(f,{op:"get_snapshot"})).hubs).some(h=>h.name==="work")).toBe(true);await action(f,{op:"setup_toggle_hub",hub:"work",enabled:false});expect(rows((await action(f,{op:"get_snapshot"})).hubs).some(h=>h.name==="work")).toBe(false);await action(f,{op:"setup_remove_hub",hub:"work"});expect(JSON.parse(await readFile(f.configPath,"utf8")).hubs.work).toBeUndefined();
});
test.each([{op:"switch_hub",from_hub:"personal",to_hub:"personal",listener_id:"c_1111111111111111"},{op:"switch_hub",from_hub:"BAD!",to_hub:"work",listener_id:"invalid"},{op:"listen_session",harness:"pi",session_id:"s1",hub:"BAD!"},{op:"listen_session",harness:"pi",session_id:"s1",scope:"invalid"},{op:"setup_apply",harness:"BAD HARNESS"},{op:"setup_remove_hub",hub:"local"},{op:"send_signal",to:"BAD TARGET",message:"hello"}])("native UI validation rejects malformed actions without persistent effects: %j",async invalid=>{
 const f=await cleanroom();await f.daemon();const before=await readFile(f.configPath,"utf8");expect((await action(f,invalid,true)).http_status).toBe(400);expect(await readFile(f.configPath,"utf8")).toBe(before);
});
