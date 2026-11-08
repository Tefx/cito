import { expect,test } from "vitest";
import { readFile,writeFile,rm } from "node:fs/promises";
import { join } from "node:path";
import { action,cleanroom,errorTool,human,rows,agent } from "../support/native-scenarios.js";

test("daemon and OIDC redirect ports validate environment and JSON aliases",async()=>{
 const f=await cleanroom();for(const port of ["","bad","0","65536","1.5"]){expect((await f.command(["find"],{env:{CITO_DAEMON_PORT:port},network:"deny"})).code).toBe(1);}
 for(const port of [-1,65536,"bad"]){await writeFile(f.configPath,JSON.stringify({daemonPort:port,hubs:{local:{url:"local://"}}}));const r=await f.command(["find"],{env:{CITO_DAEMON_PORT:undefined},network:"deny"});expect(r.code).toBe(1);expect(r.stderr).toMatch(/daemonPort|CITO_DAEMON_PORT/);}
});
test("config file loads simultaneous hubs, defaults, scopes and mirrors the selected default",async()=>{
 const f=await cleanroom({hubs:{personal:{url:"local://",user:"alice"},work:{url:"local://",user:"bob"}},defaultHub:"work"});await f.daemon();const s=await action(f,{op:"get_snapshot"});expect(rows(s.hubs).map(h=>h.name).sort()).toEqual(["personal","work"]);expect(s.hub).toBe("work");
 const work=f.query("SELECT * FROM local_listeners WHERE hub='work' AND harness='human'")[0]!;expect(work.owner_user).toBe("bob");
});
test("single named hub selects itself; multiple hubs require a valid default; explicit bad paths fail fast",async()=>{
 const f=await cleanroom({hubs:{work:{url:"local://"}},defaultHub:"work"});await f.daemon();expect((await action(f,{op:"get_snapshot"})).hub).toBe("work");
 const broken=await cleanroom();await writeFile(broken.configPath,'{"hubs":{"personal":{"url":"local://"},"work":{"url":"local://"}},"defaultHub":"missing"}');expect((await broken.command(["find"])).code).toBe(1);
});
test("no configured hubs synthesizes a zero-config local hub with symmetric unprefixed output",async()=>{
 const f=await cleanroom();await f.writeConfig({});await f.daemon();const s=await action(f,{op:"get_snapshot"});expect(rows(s.hubs)).toHaveLength(1);const enrolled=await f.tool("cito_listen",{about:"local"},human);expect(String(enrolled.listener_id)).not.toContain(":");
});
test("named and multi-hub IDs roundtrip and reject unknown or conflicting scopes",async()=>{
 const {f,p,enrolled}=await agent({hubs:{personal:{url:"local://"},work:{url:"local://"}},defaultHub:"personal"});expect(String(enrolled.listener_id)).toMatch(/^personal:/);
 expect((await errorTool(f,"cito_send",{to:enrolled.listener_id,hub:"work",message:"conflict"},human)).error).toMatchObject({code:"INVALID_INPUT"});
 expect((await errorTool(f,"cito_find",{hub:"missing"})).error).toMatchObject({code:"INVALID_INPUT"});await f.tool("cito_listen",{about:"work",hub:"work"},p.meta);
 expect(rows((await f.tool("cito_find",{hub:"work"})).listeners).every(r=>String(r.listener_id).startsWith("work:"))).toBe(true);
});
test("harness grammar is open and bounded",async()=>{
 const f=await cleanroom();await f.daemon();for(const harness of ["cursor","aider","a".repeat(32)])expect((await f.tool("cito_find",{harness})).ok).toBe(true);
 for(const harness of ["Bad Harness","a".repeat(33),"../agent",""])expect((await errorTool(f,"cito_find",{harness})).error).toMatchObject({code:"UNSUPPORTED_HARNESS"});
});
