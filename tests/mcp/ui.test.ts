import {expect,test} from "vitest";
import {readFile,writeFile} from "node:fs/promises";
import {join} from "node:path";
import {agent,remoteAgent,cleanroom,provider,seed,sendUnbound,received,action,rows,errorTool,human} from "../support/native-scenarios.js";
import {freePort,waitFor,object} from "../support/native-harness.js";
import {loginFixture} from "../support/oidc-native.js";

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
