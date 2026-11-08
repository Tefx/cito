import {afterEach} from "vitest";
import {writeFile,readFile} from "node:fs/promises";
import {join} from "node:path";
import {createHash} from "node:crypto";
import {signingKey,startOidc} from "../auth-callout/oidc-fixture.js";
import {action,cleanroom,provider} from "./native-scenarios.js";
import {object,type JsonObject} from "./native-harness.js";
const providers:Awaited<ReturnType<typeof startOidc>>[]=[];
afterEach(async()=>{for(const p of providers.splice(0).reverse())await p.stop();});
export async function loginFixture(config:JsonObject={}){
 const key=await signingKey("ES256","login"),oidc=await startOidc([key.jwk]);providers.push(oidc);const owner=await cleanroom(),hub=await owner.hub({auth:"oidc",issuer:oidc.issuer,clientId:"cito"});
 const f=await cleanroom({hubs:{work:{url:hub.url,authMode:"oidc",issuer:oidc.issuer,clientId:"cito",...config}},defaultHub:"work"});const opener=join(f.root,"browser.mjs");
 await writeFile(opener,`#!${process.execPath}\nfetch(process.argv.at(-1)).catch(e=>{process.stderr.write(String(e));process.exitCode=1;});`,{mode:0o700});
 oidc.state.tokenFactory=async nonce=>({id_token:await key.sign(oidc.claims({nonce})),refresh_token:"refresh-old"});const p=await provider(f);await f.daemon({CITO_BROWSER_BIN:opener});
 const path=join(f.stateDir,"oidc-token-work.json");
 const cache=async(token:string,refresh="refresh-old",expires=0)=>{await writeFile(path,JSON.stringify({issuer:oidc.issuer,client_id:"cito",id_token:token,refresh_token:refresh,token_sha256:createHash("sha256").update(token).digest("hex"),expires_at:expires,verified_principal_id:oidc.issuer+"#user-1",verified_user:"alice"}),{mode:0o600});};
 return {f,p,owner,hub,key,oidc,path,cache,login:()=>action(f,{op:"login_hub",hub:"work"},true,60_000),read:async()=>object(JSON.parse(await readFile(path,"utf8")))};
}
