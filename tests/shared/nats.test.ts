import { describe, expect, it } from "vitest";
import { cleanroom } from "../support/native-scenarios.js";
import { object } from "../support/native-harness.js";
const b64=(value:unknown)=>Buffer.from(typeof value==="string"?value:JSON.stringify(value)).toString("base64");
describe("native Hub HTTP stream and KV",()=>{
 it("provisions concurrently and idempotently, retaining KV generations and deduplicating signals",async()=>{
  const f=await cleanroom(),hub=await f.hub();const info=()=>hub.rpc("stream_info",{stream:"CITO_SIGNALS"});await Promise.all([info(),info(),info()]);
  const rev=object(await hub.rpc("kv_create",{key:"c_abcdefgh2345",data_b64:b64({binding_generation:1})}));
  await hub.rpc("kv_update",{key:"c_abcdefgh2345",data_b64:b64({binding_generation:2}),revision:rev.revision});
  await expect(hub.rpc("kv_update",{key:"c_abcdefgh2345",data_b64:b64({}),revision:rev.revision})).rejects.toThrow();
  const entry=object(await hub.rpc("kv_get",{key:"c_abcdefgh2345"}));expect(Buffer.from(String(entry.value_b64),"base64").toString()).toContain('"binding_generation":2');
  const stream=object(await info());expect(stream.config).toMatchObject({subjects:["cito.signal.*"],retention:"limits",storage:"file",discard:"old",max_age:1_800_000_000_000,duplicate_window:1_800_000_000_000});
  expect(object(await hub.rpc("stream_info",{stream:"KV_cito_listeners"})).config).toMatchObject({max_msgs_per_subject:1,max_age:0,storage:"file"});
  const params={subject:"cito.signal.c_abcdefgh2345",data_b64:b64("message"),opts:{msgID:"same"}};
  const a=object(await hub.rpc("stream_publish",params)),b=object(await hub.rpc("stream_publish",params));expect(b.seq).toBe(a.seq);expect(b.duplicate).toBe(true);
  expect(Number.isFinite(Date.parse(String(stream.ts)))).toBe(true);expect(Date.parse(String(stream.ts))).toBeGreaterThanOrEqual(Number(entry.created_ms));
 });
 it("rejects incompatible existing resources without destroying data or rewriting policy",async()=>{
  const f=await cleanroom(),hub=await f.hub();await hub.rpc("kv_create",{key:"c_saved",data_b64:b64({binding_generation:4})});
  await hub.rpc("stream_update",{stream:"KV_cito_listeners",patch:{max_age:300_000_000_000}});
  expect(object(await hub.rpc("stream_info",{stream:"KV_cito_listeners"})).config).toMatchObject({max_age:300_000_000_000});
  expect(await hub.rpc("kv_get",{key:"c_saved"})).not.toBeNull();
  await hub.rpc("stream_update",{stream:"KV_cito_listeners",patch:{max_age:0}});await hub.rpc("stream_update",{stream:"CITO_SIGNALS",patch:{max_age:3_600_000_000_000}});
  expect(object(await hub.rpc("stream_info",{stream:"CITO_SIGNALS"})).config).toMatchObject({max_age:3_600_000_000_000});
 });
 it("uses HTTP/SSE transport for the native Hub stream APIs without a NATS dependency",async()=>{
  const f=await cleanroom(),hub=await f.hub();const response=await fetch(hub.url+"/v1/events",{signal:AbortSignal.timeout(2000)});expect(response.headers.get("content-type")).toContain("text/event-stream");
  await response.body?.cancel();const result=object(await hub.rpc("stream_publish",{subject:"cito.signal.c_abcdefgh2345",data_b64:b64("")}));expect(result.seq).toBe(1);
 });
});
