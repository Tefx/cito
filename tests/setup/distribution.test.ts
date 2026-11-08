import {expect,test} from "vitest";
import {mkdir,writeFile,readFile,stat,copyFile,symlink} from "node:fs/promises";
import {join} from "node:path";
import bridgeEntry from "../../plugins/pi/cito-bridge.js";
import {cleanroom,action,rows} from "../support/native-scenarios.js";
import {projectRoot,object,waitFor,CITO_VERSION} from "../support/native-harness.js";
import {UiBridge,type UiToDaemonAction} from "../support/native-ui.js";

test("distribution metadata, Homebrew syntax, native manifests, relocatable Cellar shims and signed output stay coherent",async()=>{
 const f=await cleanroom();const formula=join(projectRoot,"packaging/homebrew/cito.rb");expect((await f.command(["-c",formula],{executable:"/usr/bin/ruby"})).code).toBe(0);const text=await readFile(formula,"utf8");expect(text).toContain('version "0.18.0"');expect(text).not.toMatch(/depends_on\s+["']node["']/i);expect(await readFile(join(projectRoot,"LICENSE"),"utf8")).toContain("MIT License");const pkg=JSON.parse(await readFile(join(projectRoot,"package.json"),"utf8"));expect(pkg).toMatchObject({version:CITO_VERSION,license:"MIT",dependencies:{}});
 const marketplace=JSON.parse(await readFile(join(projectRoot,".agents/plugins/marketplace.json"),"utf8"));expect(marketplace.version).toBe(CITO_VERSION);for(const id of ["codex","claude"]){const plugin=JSON.parse(await readFile(join(projectRoot,`plugins/${id}/.${id}-plugin/plugin.json`),"utf8"));expect(plugin).toMatchObject({name:"cito",version:CITO_VERSION,mcpServers:"../.mcp.json"});const mcp=JSON.parse(await readFile(join(projectRoot,`plugins/${id}/.mcp.json`),"utf8"));expect(mcp.mcpServers.cito.command).toBe("cito-mcp");}
 const hooks=JSON.parse(await readFile(join(projectRoot,"plugins/claude/hooks/hooks.json"),"utf8"));expect(Object.keys(hooks.hooks)).toEqual(["Stop","SessionStart","UserPromptSubmit"]);
 for(const prefix of ["opt/homebrew","usr/local"]){const cellar=join(f.root,prefix,"Cellar/cito/0.18.0"),native=join(cellar,"bin/cito-native"),opt=join(f.root,prefix,"opt/cito");await mkdir(join(cellar,"bin"),{recursive:true});await mkdir(join(opt,".."),{recursive:true});await copyFile(join(projectRoot,"bin/cito-native-darwin-arm64"),native);await symlink(cellar,opt);expect((await f.command(["setup","--enable","human","--no-login-item","--json"],{executable:native})).code).toBe(0);expect(await readFile(join(f.stateDir,"bin/cito"),"utf8")).toContain(join(opt,"bin/cito-native"));}
 const entry:unknown=bridgeEntry,bridge=typeof entry==="function"?entry:object(entry).default;if(typeof bridge!=="function")throw new Error("Pi bridge is not callable");const registrations:unknown[][]=[];bridge({on(){},sendMessage(){},registerMcpServer(...args:unknown[]){registrations.push(args);}});expect(registrations).toEqual([["cito",{command:expect.stringMatching(/cito-mcp$/),args:[]}]]);expect(()=>bridge({on(){},sendMessage(){},registerMcpServer(){throw new Error("Not ready");}})).not.toThrow();
},30000);
test("first launch mutates no integrations and surgical disconnect/uninstall preserve foreign configuration",async()=>{
 const f=await cleanroom(),codex=join(f.env.CITO_CODEX_HOME!,"config.toml"),pi=join(f.env.CITO_PI_HOME!,"settings.json"),claude=join(f.env.CITO_CLAUDE_HOME!,"settings.json");await mkdir(f.env.CITO_PI_HOME!,{recursive:true});const toml='# User comment\nmodel="gpt-5"\n[mcp_servers.custom]\ncommand="custom-mcp"\n';const initial={theme:"dark",mcpServers:{other:{command:"other-mcp",args:[]}},hooks:{Stop:[{hooks:[{type:"command",command:"user-stop-hook"}]}]}};await writeFile(codex,toml);await writeFile(pi,JSON.stringify({theme:"solarized",packages:["npm:other-pkg","npm:@tefx/cito"]}));await writeFile(claude,JSON.stringify(initial));const daemon=await f.daemon();expect(await readFile(codex,"utf8")).toBe(toml);expect(JSON.parse(await readFile(claude,"utf8"))).toEqual(initial);await expect(stat(join(f.home,"Library/LaunchAgents/com.tefx.cito.daemon.plist"))).rejects.toMatchObject({code:"ENOENT"});
 for(const harness of ["pi","codex","claude"])expect((await action(f,{op:"setup_apply",harness})).http_status).toBe(200);const installed=JSON.parse(await readFile(pi,"utf8"));expect(installed.theme).toBe("solarized");expect(installed.packages).toContain("npm:other-pkg");expect(installed.packages).not.toContain("npm:@tefx/cito");expect(installed.packages).toContain(join(f.stateDir,"plugins/pi"));
 await action(f,{op:"setup_remove_app",harness:"codex"});expect(await readFile(codex,"utf8")).toBe(toml);await writeFile(codex,toml+'\n[mcp_servers.cito]\ncommand="cito-mcp"\n[mcp_servers.cito.env]\nTEST="value"\n[mcp_servers.after]\ncommand="after"\n');await action(f,{op:"setup_remove_app",harness:"codex"});const removed=await readFile(codex,"utf8");expect(removed).not.toContain("mcp_servers.cito");expect(removed).toContain("[mcp_servers.after]");await action(f,{op:"setup_remove_app",harness:"claude"});expect(JSON.parse(await readFile(claude,"utf8"))).toEqual(initial);await action(f,{op:"setup_remove_app",harness:"pi"});expect(JSON.parse(await readFile(pi,"utf8")).packages).toEqual(["npm:other-pkg"]);
 await daemon.stop();await writeFile(join(f.stateDir,"retain-data"),"keep");expect((await f.command(["setup","--uninstall","--json"])).code).toBe(0);expect(await readFile(join(f.stateDir,"retain-data"),"utf8")).toBe("keep");expect((await f.command(["setup","--uninstall","--purge-data","--json"])).code).toBe(0);await expect(stat(f.stateDir)).rejects.toMatchObject({code:"ENOENT"});expect(JSON.parse(await readFile(claude,"utf8"))).toEqual(initial);
},30000);
test("actual SettingsPanel requires setup consent, emits per-app Disconnect and confirms global Remove All",async()=>{
 const f=await cleanroom(),actions:UiToDaemonAction[]=[],diagnostics:string[]=[];
 const uiEnv={CITO_UI_HEADLESS:"0"};
 const options={binary:join(projectRoot,"bin/cito-ui-darwin-arm64"),env:uiEnv,onAction:(a:UiToDaemonAction)=>{actions.push(a);},diagnostic:(l:string)=>diagnostics.push(l)};
 let bridge=new UiBridge(f.stateDir,options);
 const settings={apps:[{id:"codex",display_name:"Codex",tier:"stable" as const,installed:true,configured:false,status:"not_configured" as const,detail:"Click Set Up to connect",paths:[]}],hubs:[],host_hub:{enabled:false,running:false,port:1,endpoint_url:"http://127.0.0.1:1",token:"",invite_command:"",error:null},launch_at_login:false};
 const send=(value:object)=>Reflect.get(bridge,"child").stdin.write(JSON.stringify(value)+"\n");
 const inspect=async()=>{
  const before=diagnostics.filter(l=>l.startsWith("cito-ui: menu ")).length;send({op:"test_inspect"});
  await waitFor(()=>diagnostics.filter(l=>l.startsWith("cito-ui: menu ")).length>before);
  return object(JSON.parse(diagnostics.filter(l=>l.startsWith("cito-ui: menu ")).at(-1)!.slice("cito-ui: menu ".length)));
 };
 const flatten=(nodes:Record<string,unknown>[]):Record<string,unknown>[]=>nodes.flatMap(node=>[node,...flatten(Array.isArray(node.children)?rows(node.children):[])]);
 const has=async(id:string,symbol?:string)=>flatten(rows((await inspect()).settings_items)).some(n=>n.id===id&&(!symbol||n.symbol===symbol));
 try{
  await bridge.start();bridge.state([{name:"local",status:"connected",user:"alice",auth_mode:"none",error:null}],[],[],[],0,[],settings);
  await waitFor(()=>actions.some(a=>a.op==="open_settings"));await waitFor(()=>stat(join(f.stateDir,"onboarding-shown")).then(s=>s.isFile(),()=>false));
  expect(actions.some(a=>a.op==="setup_apply")).toBe(false);
  await bridge.close();uiEnv.CITO_UI_HEADLESS="1";bridge=new UiBridge(f.stateDir,options);await bridge.start();
  settings.apps[0]!.configured=true;settings.apps[0]!.status="ready" as "not_configured";
  bridge.state([{name:"local",status:"connected",user:"alice",auth_mode:"none",error:null}],[],[],[],0,[],settings);
  send({op:"test_control",id:"open_settings"});
  await waitFor(()=>has("remove_app:codex"));send({op:"test_control",id:"remove_app:codex"});
  await waitFor(()=>actions.some(a=>a.op==="setup_remove_app"&&a.harness==="codex"));
  send({op:"test_control",id:"open_uninstall_confirm"});await waitFor(()=>has("confirm_uninstall_all"));
  expect(actions.some(a=>a.op==="setup_uninstall_all")).toBe(false);
  send({op:"test_control",id:"cancel_uninstall_all"});await waitFor(async()=>!await has("confirm_uninstall_all"));
  expect(actions.some(a=>a.op==="setup_uninstall_all")).toBe(false);
  send({op:"test_control",id:"open_uninstall_confirm"});await waitFor(()=>has("uninstall_purge_data"));
  send({op:"test_control",id:"uninstall_purge_data"});await waitFor(()=>has("uninstall_purge_data","checkmark.square.fill"));
  send({op:"test_control",id:"confirm_uninstall_all"});await waitFor(()=>actions.some(a=>a.op==="setup_uninstall_all"&&a.purge_data===true));
 }finally{await bridge.close();}
},20000);
test("native app, setup, Hub, MCP and relocated hook execute in a zero-Node cleanroom",async()=>{
 const f=await cleanroom(),env={PATH:"/usr/bin:/bin:/usr/sbin:/sbin",CITO_NODE_BIN:""};expect((await f.command(["node"],{executable:"/usr/bin/which",env})).code).not.toBe(0);expect((await f.command(["setup","--enable","claude","--no-login-item","--json"],{env})).code).toBe(0);const built=await f.command(["setup","--build-app",join(f.root,"Apps"),"--json"],{env,timeout:30000});expect(built.code).toBe(0);const app=JSON.parse(built.stdout).appPath;for(const executable of [join(app,"Contents/MacOS/Cito"),join(f.stateDir,"bin/cito")])expect((await f.command(["--version"],{executable,env})).stdout).toBe("cito v0.18.0\n");const hub=await f.hub({env});expect(await hub.rpc("account_info")).toHaveProperty("streams");await f.daemon(env);const mcp=await f.command([],{executable:join(f.stateDir,"bin/cito-mcp"),env,input:JSON.stringify({jsonrpc:"2.0",id:1,method:"tools/list"})+"\n"});expect(mcp.code).toBe(0);expect(mcp.stdout).toContain("cito_listen");const hook=await f.command([],{executable:join(f.stateDir,"plugins/claude/bin/cito-claude-hook"),env,input:JSON.stringify({hook_event_name:"Stop",cwd:f.root})});expect(hook).toMatchObject({code:0,stdout:"{}\n"});
},45000);
