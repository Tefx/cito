import {expect,test} from "vitest";
import {cleanroom,wire} from "../support/native-scenarios.js";
import {object} from "../support/native-harness.js";
import {signingKey,startOidc} from "./oidc-fixture.js";
const enc=(x:unknown)=>Buffer.from(JSON.stringify(x)).toString("base64");
test("pre-shared token mode rejects missing/wrong bearer tokens and accepts its configured token",async()=>{const f=await cleanroom(),hub=await f.hub({auth:"token",token:"fixture-token"});expect(await hub.rpc("account_info")).toHaveProperty("streams");for(const authorization of ["","Bearer wrong","Basic fixture-token"])await expect(hub.rpc("account_info",{},{authorization})).rejects.toMatchObject({status:401});});
test("OIDC discovery failure returns transient AUTH_UNAVAILABLE without crashing the Hub",async()=>{const key=await signingKey("ES256","unavailable"),oidc=await startOidc([key.jwk]),f=await cleanroom();oidc.state.fail=true;try{const hub=await f.hub({auth:"oidc",issuer:oidc.issuer,clientId:"cito"});await expect(hub.rpc("account_info",{},{authorization:"Bearer "+await key.sign(oidc.claims())})).rejects.toMatchObject({code:"AUTH_UNAVAILABLE"});expect((await fetch(hub.url+"/health")).status).toBe(200);}finally{await oidc.stop();}});
test("OIDC row security protects KV, signal consumers and claims, rejects wildcard subjects, and records audits",async()=>{
 const key=await signingKey("ES256","key"),oidc=await startOidc([key.jwk]),f=await cleanroom();try{const hub=await f.hub({auth:"oidc",issuer:oidc.issuer,clientId:"cito"});const alice={authorization:"Bearer "+await key.sign(oidc.claims({sub:"alice",preferred_username:"Alice"}))},bob={authorization:"Bearer "+await key.sign(oidc.claims({sub:"bob",preferred_username:"Bob"}))};
 const id="c_aaaaaaaaaaaaaaaa",reg={listener_id:id,public_id:"alice-aaaaaaaaaaaaaaaa",harness:"cursor",session_id:"s",owner_user:"alice",scope:"self",state:"ACTIVE",binding_generation:1};await hub.rpc("kv_create",{key:id,data_b64:enc(reg)},alice);expect(await hub.rpc("kv_get",{key:id},bob)).toMatchObject({forbidden_scope:true});
 for(const op of ["kv_put","kv_update","kv_delete"])await expect(hub.rpc(op,{key:id,data_b64:enc(reg),revision:1},bob)).rejects.toThrow();
 await expect(hub.rpc("stream_publish",{subject:"cito.signal."+id,data_b64:enc(wire("private",id,"none#bob"))},bob)).rejects.toThrow();
 const entry=object(await hub.rpc("kv_get",{key:id},alice));await hub.rpc("kv_update",{key:id,revision:entry.revision,data_b64:enc({...reg,scope:"workspace"})},alice);const seq=object(await hub.rpc("stream_publish",{subject:"cito.signal."+id,data_b64:enc(wire("workspace",id,oidc.issuer+"#bob"))},bob));expect(seq.seq).toBeGreaterThan(0);
 await expect(hub.rpc("consumer_add",{stream:"CITO_SIGNALS",config:{durable_name:"other",filter_subject:"cito.signal."+id}},bob)).rejects.toThrow();
 for(const subject of ["other.subject","cito.signal.*","cito.signal.>"])await expect(hub.rpc("stream_publish",{subject,data_b64:enc({})},alice)).rejects.toThrow();
 await expect(hub.rpc("signal_claim_dispatch",{node_id:id,channel_id:"other",signal_ids:["workspace"]},bob)).rejects.toThrow();await expect(hub.rpc("signal_ack",{node_id:id,signal_ids:["workspace"]},bob)).rejects.toThrow();
 const audits=f.query("SELECT * FROM hub_audit_log",[],"hub/hub.sqlite");expect(audits.length).toBeGreaterThanOrEqual(3);expect(JSON.stringify(audits)).not.toContain(alice.authorization);
 }finally{await oidc.stop();}
});
