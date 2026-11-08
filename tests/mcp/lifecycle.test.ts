import {expect,test} from "vitest";
import {readFile} from "node:fs/promises";
import {agent,remoteAgent,cleanroom,provider,seed,sendUnbound,received,action,rows,errorTool,wire,inject,human} from "../support/native-scenarios.js";
import {waitFor,object,bareId} from "../support/native-harness.js";

test("restart restores ACTIVE ownership and drains frozen then queued batches without re-enrolling",async()=>{
 const {f,p,row,id,daemon}=await agent();const initial=f.query("SELECT binding_generation,owner_token FROM local_listeners WHERE listener_id=?",[id])[0];
 await daemon.stop();seed(f,row,"frozen",1,{state:"DISPATCHING",batch_id:"frozen-before-restart",queue_written:1});seed(f,row,"new",2);
 // Restart the owned daemon through its shutdown action, retaining the private cleanroom.
 await f.daemon();
 await waitFor(async()=>(await p.calls()).filter(c=>c.op==="wake").length>=2,10000);const wakes=(await p.calls()).filter(c=>c.op==="wake");expect(wakes[0]!.batch_id).toBe("frozen-before-restart");expect(wakes[1]!.batch_id).not.toBe(wakes[0]!.batch_id);expect(f.query("SELECT binding_generation,owner_token FROM local_listeners WHERE listener_id=?",[id])[0]).toEqual(initial);
});
test("unlisten cleanup cannot delete a concurrently installed replacement receiver",async()=>{
 const {f,p,proxy,id,row}=await remoteAgent();proxy.hold("kv_update");const pending=f.tool("cito_unlisten",{},p.meta).catch(e=>e);await waitFor(()=>proxy.calls.some(c=>c.op==="kv_update"));
 f.mutate("UPDATE local_listeners SET owner_token='replacement',binding_generation=binding_generation+1,status='ACTIVE' WHERE listener_id=?",[id]);proxy.release("kv_update");await pending;expect(f.query("SELECT status,owner_token FROM local_listeners WHERE listener_id=?",[id])[0]).toEqual({status:"ACTIVE",owner_token:"replacement"});
});
test("remote revocation failure preserves local unlisten and suppresses pending wakes",async()=>{
 const {f,p,proxy,id,enrolled}=await remoteAgent();await sendUnbound(f,String(enrolled.listener_id),"passive");proxy.fail("kv_update",10);await f.tool("cito_unlisten",{},p.meta).catch(()=>{});expect(f.query("SELECT status FROM local_listeners WHERE listener_id=?",[id])[0]!.status).toBe("UNLISTENED");expect(f.query("SELECT state FROM local_signals WHERE listener_id=?",[id]).every(r=>r.state==="STORED_PASSIVE")).toBe(true);
});
test("Pi A to B to A retains metadata without wake quotas",async()=>{
 const f=await cleanroom();const pi=await f.piBridge("A");await f.daemon();for(const session of ["A","B","A"]){await pi.switchSession(session);const listen=await f.tool("cito_listen",{about:session},pi.meta);expect(listen.session_id).toBe(session);}expect(f.query("SELECT DISTINCT session_id FROM local_listeners WHERE harness='pi'").map(r=>r.session_id).sort()).toEqual(["A","B"]);
});
test.each(["flush","ack"])("shutdown interrupts a stalled send %s boundary without waiting for its deadline",async boundary=>{
 const {f,proxy,daemon,enrolled}=await remoteAgent();const op=boundary==="flush"?"js_publish":"consumer_backlog";proxy.hold(op);const pending=sendUnbound(f,String(enrolled.listener_id)).catch(e=>e);await waitFor(()=>proxy.calls.some(c=>c.op===op));const began=performance.now();await daemon.stop();expect(performance.now()-began).toBeLessThan(1500);proxy.release(op);await pending;
});
test("a stalled publish is bounded by the ten-second operation deadline",async()=>{
 const {f,proxy,enrolled}=await remoteAgent();proxy.hold("js_publish");const began=performance.now();const result=await sendUnbound(f,String(enrolled.listener_id)).catch(e=>e);expect(result).toBeInstanceOf(Error);expect(performance.now()-began).toBeLessThan(11000);proxy.release("js_publish");
},15000);
test("scheduled liveness sweep renews a live registration without user calls",async()=>{
 const {f,id}=await agent();const initial=f.query("SELECT revision FROM hub_kv WHERE key=?",[id],"hub.sqlite")[0]!.revision;await waitFor(()=>Number(f.query("SELECT revision FROM hub_kv WHERE key=?",[id],"hub.sqlite")[0]!.revision)>Number(initial),22000);
},25000);
test("transient probes revoke on the third failure while healthy probes reset the streak",async()=>{
 const {f,p,id}=await agent();await p.set({status:"transient",reason:"busy"});await waitFor(()=>f.query("SELECT status FROM local_listeners WHERE listener_id=?",[id])[0]!.status==="UNLISTENED",5000);expect((await p.calls()).filter(c=>c.op==="probe")).toHaveLength(3);
});
test("definitive no-client-found revokes immediately and keeps unread passive signals",async()=>{
 const {f,p,id,row}=await agent();seed(f,row,"unread");await p.set({status:"closed_definitive"});await waitFor(()=>f.query("SELECT status FROM local_listeners WHERE listener_id=?",[id])[0]!.status==="UNLISTENED");expect(f.query("SELECT acknowledged_at_ms,state FROM local_signals WHERE signal_id='unread'")[0]).toMatchObject({acknowledged_at_ms:null,state:"STORED_PASSIVE"});
});
test("CAS retry exhaustion fences a failed contender without advancing local generation",async()=>{
 const {f,p,proxy,id,row}=await remoteAgent();proxy.fail("kv_update",3);expect((await errorTool(f,"cito_listen",{about:"contender"},p.meta)).error).toMatchObject({code:"STALE_GENERATION"});expect(f.query("SELECT binding_generation,owner_token FROM local_listeners WHERE listener_id=?",[id])[0]).toMatchObject({binding_generation:row.binding_generation,owner_token:row.owner_token});
});
test("find drains a full KV snapshot and applies the fifty-result limit after sorting",async()=>{
 const {f,hub}=await remoteAgent();for(let i=60;i>=1;i--)await hub.rpc("kv_put",{key:"c_"+i,data_b64:Buffer.from(JSON.stringify({listener_id:"c_"+i,public_id:"peer-"+String(i).padStart(3,"0"),harness:"cursor",state:"ACTIVE",scope:"workspace",owner_user:"alice",principal_id:"none#alice"})).toString("base64")});const found=rows((await f.tool("cito_find",{limit:50,harness:"cursor"})).listeners);expect(found).toHaveLength(50);expect(found.map(r=>r.listener_id)).toEqual(found.map(r=>r.listener_id).sort());
});
test("a second MCP process revokes the active owner without losing pending-signal suppression",async()=>{
 const {f,p,id,row}=await agent();seed(f,row,"pending");const result=await f.command(["unlisten","--harness","cursor","--session","session-1"]);expect(result.code).toBe(0);expect(f.query("SELECT status FROM local_listeners WHERE listener_id=?",[id])[0]!.status).toBe("UNLISTENED");expect(f.query("SELECT held_reason,state FROM local_signals WHERE signal_id='pending'")[0]).toMatchObject({state:"STORED_PASSIVE",held_reason:"unlistened"});
});
test("reconnection consumes from persisted cursor without duplicate admission",async()=>{
 const {f,p,proxy,enrolled,id}=await remoteAgent();await sendUnbound(f,String(enrolled.listener_id));await received(f,id);proxy.fail("consumer_backlog",2);proxy.restore();await sendUnbound(f,String(enrolled.listener_id),"second");await received(f,id,2);expect(f.query("SELECT COUNT(DISTINCT signal_id) AS n FROM local_signals WHERE listener_id=?",[id])[0]!.n).toBe(2);
});
test("admission failure cannot advance the cursor beyond a failed earlier signal",async()=>{
 const {f,enrolled,id}=await agent();f.mutate("CREATE TRIGGER reject_admit BEFORE INSERT ON local_signals BEGIN SELECT RAISE(ABORT,'fixture failure');END;");await sendUnbound(f,String(enrolled.listener_id)).catch(()=>{});expect(f.query("SELECT persisted_seq FROM local_listeners WHERE listener_id=?",[id])[0]?.persisted_seq??0).toBe(0);f.mutate("DROP TRIGGER reject_admit");await received(f,id);
});
test("a disconnected recipient reports the exact sender timeout and passive reason",async()=>{
 const {f,enrolled,hub}=await remoteAgent();await action(f,{op:"hub_action",hub:"work",action:"disconnect"});
 const sender=await cleanroom({hubs:{work:{url:hub.url,authMode:"none",user:"alice"}},defaultHub:"work"});await sender.daemon();
 const result=await sendUnbound(sender,String(enrolled.listener_id));expect(result.status).toBe("stored_passive");expect(result.held_reason).toBe("sender_ack_timeout");expect(f.query("SELECT * FROM local_signals WHERE signal_id=?",[bareId(result.signal_id)])).toEqual([]);
},12000);
test("symmetric human nodes attach multiple channels, preserve siblings and synchronize business acknowledgments",async()=>{
 const owner=await cleanroom(),hub=await owner.hub();const config={hubs:{work:{url:hub.url,authMode:"none",user:"alice"}},defaultHub:"work"};const a=await cleanroom(config),b=await cleanroom(config);const p=await provider(a);await a.daemon();await b.daemon();await action(a,{op:"listen_session",harness:"cursor",session_id:"session-1"});
 const humanA=a.query("SELECT * FROM local_listeners WHERE harness='human'")[0]!,humanB=b.query("SELECT * FROM local_listeners WHERE harness='human'")[0]!;expect(humanA.listener_id).toBe(humanB.listener_id);expect(humanB.status).toBe("ACTIVE");const entry=object(await hub.rpc("kv_get",{key:humanA.listener_id}));const registration=object(JSON.parse(Buffer.from(String(entry.value_b64),"base64").toString()));expect(Object.keys(object(registration.channels))).toHaveLength(2);
 const sent=await a.tool("cito_send",{to:"@self",message:"cross-channel"},p.meta);await received(a,String(humanA.listener_id));await received(b,String(humanB.listener_id));await action(b,{op:"hub_action",hub:"work",action:"disconnect"});await action(a,{op:"ack_signals",signal_ids:[sent.signal_id]});expect(b.query("SELECT acknowledged_at_ms FROM local_signals WHERE signal_id=?",[bareId(sent.signal_id)])[0]!.acknowledged_at_ms).toBeNull();await action(b,{op:"hub_action",hub:"work",action:"reconnect"});
 await waitFor(()=>{const rows=b.query("SELECT * FROM local_signals WHERE signal_id=?",[bareId(sent.signal_id)]);return rows.length===1&&typeof rows[0]?.acknowledged_at_ms==="number";});
 const parsed=rows((await a.tool("cito_find",{user:"alice",harness:"human"})).listeners);expect(parsed).toHaveLength(1);await a.tool("cito_unlisten",{},human);expect(b.query("SELECT status FROM local_listeners WHERE harness='human'")[0]!.status).toBe("ACTIVE");
});
