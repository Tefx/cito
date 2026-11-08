import {expect,test} from "vitest";
import {agent,cleanroom,seed,sendUnbound,received,rows,errorTool,wire,inject,action} from "../support/native-scenarios.js";
import {waitFor,object} from "../support/native-harness.js";

test("V22 public IDs normalize owner handles to at most 32 characters and use stable hex Node IDs",async()=>{
 for(const [user,slug]of [["alice.smith","alice-smith"],["alice_smith","alice-smith"],["a".repeat(40),"a".repeat(32)]]){const f=await cleanroom({hubs:{local:{url:"local://",user}},defaultHub:"local"});await f.daemon();const r=await f.tool("cito_listen",{about:"Identity"},{expected_harness:"human",expected_session_id:"default"});expect(r.canonical_listener_id).toMatch(/^c_[a-f0-9]{16}$/);expect(r.listener_id).toBe(slug+"-"+String(r.canonical_listener_id).slice(2));}
});
test("tag renaming preserves canonical KV, subjects, SQLite keys and sender idempotency",async()=>{
 const {f,p,id,enrolled}=await agent();const sent=await sendUnbound(f,String(enrolled.listener_id),"same",{idempotency_key:"rename"});const before=f.query("SELECT binding_generation FROM local_listeners WHERE listener_id=?",[id])[0];const renamed=await f.tool("cito_listen",{about:"same",tag:"renamed"},p.meta);expect(renamed.canonical_listener_id).toBe(id);expect(renamed.listener_id).toBe(enrolled.listener_id);const again=await sendUnbound(f,String(renamed.listener_id),"same",{idempotency_key:"rename"});expect(again.signal_id).toBe(sent.signal_id);expect(f.query("SELECT binding_generation FROM local_listeners WHERE listener_id=?",[id])[0]).toEqual(before);expect(f.query("SELECT subject FROM hub_signals",[],"hub.sqlite").every(r=>r.subject==="cito.signal."+id)).toBe(true);
});
test("invalid explicit tags leave live ownership and persisted registration untouched",async()=>{
 const {f,p,id}=await agent();const before=f.query("SELECT * FROM local_listeners WHERE listener_id=?",[id])[0];for(const tag of ["","   ","x".repeat(65),42]){expect((await errorTool(f,"cito_listen",{about:"must not change",tag},p.meta)).error).toMatchObject({code:"INVALID_INPUT"});expect(f.query("SELECT * FROM local_listeners WHERE listener_id=?",[id])[0]).toEqual(before);}
});
test.each(["principal_id","harness","session_id"])("collision guard checks %s before stopping an owner or writing SQLite",async field=>{
 const {f,p,id}=await agent();const before=f.query("SELECT * FROM local_listeners WHERE listener_id=?",[id])[0]!;const raw=f.query("SELECT CAST(value AS TEXT) AS payload FROM hub_kv WHERE key=?",[id],"hub.sqlite")[0]!;const registration=object(JSON.parse(String(raw.payload)));registration[field]="collision";f.mutate("UPDATE hub_kv SET value=CAST(? AS BLOB) WHERE key=?",[JSON.stringify(registration),id],"hub.sqlite");expect((await errorTool(f,"cito_listen",{about:"must not replace"},p.meta)).error).toMatchObject({code:"STALE_GENERATION"});expect(f.query("SELECT * FROM local_listeners WHERE listener_id=?",[id])[0]).toEqual(before);
});
test("V4 rows persist public IDs and ownership across native daemon reopening",async()=>{
 const {f,id,daemon}=await agent();const before=f.query("SELECT * FROM local_listeners WHERE listener_id=?",[id])[0];await action(f,{op:"quit"});await daemon.result;await f.daemon();expect(f.query("SELECT * FROM local_listeners WHERE listener_id=?",[id])[0]).toEqual(before);
});
test("badges reflect waking/passive admissions, acknowledgments and restore",async()=>{
 const {f,p,row,id}=await agent();seed(f,row,"one",1,{state:"STORED_PASSIVE",held_reason:"stale_generation"});await waitFor(async()=>(await p.calls()).some(c=>c.op==="update_badge"&&String(c.badge).includes("(1)")));await f.tool("cito_inbox",{},p.meta);await waitFor(async()=>(await p.calls()).some(c=>c.op==="update_badge"&&!String(c.badge).includes("(1)")));expect(typeof f.query("SELECT acknowledged_at_ms FROM local_signals WHERE signal_id='one'")[0]!.acknowledged_at_ms).toBe("number");
});
test("persisted ACTIVE rows with no live receiver cannot contribute a badge",async()=>{
 const {f,p,id}=await agent();await action(f,{op:"hub_action",hub:"local",action:"disconnect"});const count=(await p.calls()).length;await new Promise(r=>setTimeout(r,400));expect((await p.calls()).slice(count).filter(c=>c.op==="update_badge"&&c.badge!==null)).toEqual([]);expect(f.query("SELECT status FROM local_listeners WHERE listener_id=?",[id])[0]!.status).toBe("ACTIVE");
});
test("a fenced receiver cannot mutate its replacement's persisted owner",async()=>{
 const {f,id}=await agent();f.mutate("UPDATE local_listeners SET owner_token='replacement',binding_generation=binding_generation+1 WHERE listener_id=?",[id]);await new Promise(r=>setTimeout(r,700));expect(f.query("SELECT owner_token FROM local_listeners WHERE listener_id=?",[id])[0]!.owner_token).toBe("replacement");
});
test("public wire recipients are admitted only from their canonical subjects",async()=>{
 const {f,id,row,enrolled}=await agent();const signal=wire("public-recipient",String(enrolled.listener_id),String(row.principal_id),Number(row.binding_generation));f.mutate("INSERT INTO hub_signals(seq,subject,msg_id,signal_id,created_ms,data) VALUES(1,?,?,?, ?,CAST(? AS BLOB))",["cito.signal."+id,"public-wire","public-recipient",Date.now(),JSON.stringify(signal)],"hub.sqlite");await waitFor(()=>f.query("SELECT * FROM local_signals WHERE listener_id=?",[id]).length===1);expect(f.query("SELECT listener_id FROM local_signals WHERE signal_id='public-recipient'")[0]!.listener_id).toBe(id);
});
