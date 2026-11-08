import {expect,test} from "vitest";
import {remoteAgent,action,sendUnbound,received,rows} from "../support/native-scenarios.js";
import {waitFor} from "../support/native-harness.js";

test("failed reconnect KV reads retry restoration and remain monitored across later cycles",async()=>{
 const {f,p,proxy,enrolled,id}=await remoteAgent();proxy.fail("kv_get",2);await action(f,{op:"hub_action",hub:"work",action:"disconnect"});proxy.restore();await action(f,{op:"hub_action",hub:"work",action:"reconnect"});await sendUnbound(f,String(enrolled.listener_id));await received(f,id);await action(f,{op:"hub_action",hub:"work",action:"disconnect"});await action(f,{op:"hub_action",hub:"work",action:"reconnect"});await sendUnbound(f,String(enrolled.listener_id),"again");await received(f,id,2);
});
test("disconnect cancels a pending restoration retry",async()=>{
 const {f,proxy,id}=await remoteAgent();proxy.fail("kv_get",20);await action(f,{op:"hub_action",hub:"work",action:"disconnect"});const count=proxy.calls.length;await new Promise(r=>setTimeout(r,1700));expect(proxy.calls.length).toBe(count);expect(f.query("SELECT status FROM local_listeners WHERE listener_id=?",[id])[0]!.status).toBe("ACTIVE");
});
test("real Hub authorization rejection becomes auth_required without another tool call",async()=>{
 const {f,proxy}=await remoteAgent();proxy.fail("consumer_add",100,401);await waitFor(async()=>rows((await action(f,{op:"get_snapshot"})).hubs).some(h=>h.status==="auth_required"));
});
test("unexpected transport closure becomes disconnected and cancels active wakes",async()=>{
 const {f,proxy,id}=await remoteAgent();await proxy.close();await waitFor(async()=>rows((await action(f,{op:"get_snapshot"})).hubs).some(h=>["disconnected","reconnecting"].includes(String(h.status))));expect(f.query("SELECT status FROM local_listeners WHERE listener_id=?",[id])[0]!.status).toBe("ACTIVE");
});
