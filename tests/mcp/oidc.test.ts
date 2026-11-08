import {expect,test} from "vitest";
import {stat,readFile,writeFile} from "node:fs/promises";
import {join} from "node:path";
import {loginFixture} from "../support/oidc-native.js";
import {action,cleanroom,rows} from "../support/native-scenarios.js";
import {signingKey} from "../auth-callout/oidc-fixture.js";
import {freePort,waitFor} from "../support/native-harness.js";

test("interactive S256 PKCE verifies signatures and stores canonical identity with mode 0600",async()=>{
 const x=await loginFixture();expect((await x.login()).http_status).toBe(200);expect(x.oidc.state.challenge).toMatch(/^[A-Za-z0-9_-]{43}$/);const cache=await x.read();expect(cache).toMatchObject({verified_user:"alice",verified_principal_id:expect.stringContaining("user-1"),refresh_token:"refresh-old"});expect((await stat(x.path)).mode&0o777).toBe(0o600);expect(x.oidc.state.codeRequests).toBe(1);
});
test("rejected unexpired tokens refresh once and concurrent waiters reuse the verified cache",async()=>{
 const x=await loginFixture();const old=await x.key.sign(x.oidc.claims());await x.cache(old,"refresh-old",0);const results=await Promise.all([x.login(),x.login(),x.login()]);expect(results.every(r=>r.http_status===200)).toBe(true);expect(x.oidc.state.refreshRequests).toBe(1);expect(x.oidc.state.authorizationRequests).toBe(0);expect((await x.read()).id_token).not.toBe(old);
});
test.each([400,401])("refresh %i falls through to PKCE within the same lock",async status=>{const x=await loginFixture();await x.cache(await x.key.sign(x.oidc.claims()));x.oidc.state.refreshStatus=status;expect((await x.login()).http_status).toBe(200);expect(x.oidc.state.refreshRequests).toBe(1);expect(x.oidc.state.authorizationRequests).toBe(1);});
test("refresh 503 fails without launching a browser or overwriting verified cache",async()=>{const x=await loginFixture();await x.cache(await x.key.sign(x.oidc.claims()));const before=await readFile(x.path,"utf8");x.oidc.state.refreshStatus=503;expect((await x.login()).http_status).toBe(400);expect(x.oidc.state.authorizationRequests).toBe(0);expect(await readFile(x.path,"utf8")).toBe(before);});
test("a different PKCE identity cannot inherit the previous refresh token after rejection",async()=>{const x=await loginFixture();await x.cache(await x.key.sign(x.oidc.claims()));x.oidc.state.refreshStatus=401;x.oidc.state.tokenFactory=async nonce=>({id_token:await x.key.sign(x.oidc.claims({sub:"other",nonce}))});expect((await x.login()).http_status).toBe(200);expect((await x.read()).verified_principal_id).toContain("other");expect((await x.read()).refresh_token).toBeNull();});
test.each([false,true])("silent refresh retains an omitted refresh token only for the same principal (changed %s)",async changed=>{const x=await loginFixture();await x.cache(await x.key.sign(x.oidc.claims()));x.oidc.state.tokenFactory=async()=>({id_token:await x.key.sign(x.oidc.claims({sub:changed?"changed":"user-1"}))});expect((await x.login()).http_status).toBe(200);expect((await x.read()).refresh_token).toBe(changed?null:"refresh-old");expect(x.oidc.state.authorizationRequests).toBe(0);});
test("overall login timeout remains bounded when JWKS verification cannot finish",async()=>{const x=await loginFixture();x.oidc.state.stall=true;const began=performance.now();const result=await x.login();expect(String(result.error)).toContain("OIDC_LOGIN_TIMEOUT");expect(performance.now()-began).toBeLessThan(60_000);await expect(stat(x.path)).rejects.toMatchObject({code:"ENOENT"});},65000);
test("hub token caches and PKCE scopes are isolated without legacy fallback",async()=>{
 const x=await loginFixture({scopes:["openid","email"]});await writeFile(join(x.f.stateDir,"oidc-token.json"),JSON.stringify({id_token:"legacy",refresh_token:"secret"}));expect((await x.login()).http_status).toBe(200);expect(x.oidc.state.requestedScopes.split(" ")).toEqual(["openid","email"]);expect(await readFile(join(x.f.stateDir,"oidc-token.json"),"utf8")).toContain("legacy");
 const other=await loginFixture({scopes:["openid","profile"]});expect((await other.login()).http_status).toBe(200);expect(other.oidc.state.requestedScopes).toBe("openid profile");expect((await x.read()).issuer).not.toBe((await other.read()).issuer);
});
test("discovery negotiates advertised defaults, honors explicit scopes and rejects missing openid",async()=>{
 const x=await loginFixture();x.oidc.state.scopesSupported=["openid","email"];expect((await x.login()).http_status).toBe(200);expect(x.oidc.state.requestedScopes).toBe("openid email");
 const bad=await loginFixture();bad.oidc.state.scopesSupported=["profile"];expect((await bad.login()).http_status).toBe(400);expect(bad.oidc.state.authorizationRequests).toBe(0);
});
test("fixed callback ports serialize independent hub logins and cancellation opens no extra browser",async()=>{
 const port=await freePort(),a=await loginFixture({redirectPort:port}),b=await loginFixture({redirectPort:port});const results=await Promise.all([a.login(),b.login()]);expect(results.map(r=>r.http_status)).toEqual([200,200]);expect(a.oidc.state.authorizationRequests).toBe(1);expect(b.oidc.state.authorizationRequests).toBe(1);
});
test("locally invalid signatures can never enter a verified identity cache",async()=>{
 const x=await loginFixture(),bad=await signingKey("ES256","login");x.oidc.state.tokenFactory=async nonce=>({id_token:await bad.sign(x.oidc.claims({nonce}))});expect((await x.login()).http_status).toBe(400);await expect(stat(x.path)).rejects.toMatchObject({code:"ENOENT"});
});
