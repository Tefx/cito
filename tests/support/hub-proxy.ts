import {createServer} from "node:http";
import {Readable} from "node:stream";
import {pipeline} from "node:stream/promises";
import {afterEach} from "vitest";
import {object,type JsonObject} from "./native-harness.js";
const closers:(()=>Promise<void>)[]=[];
afterEach(async()=>{for(const close of closers.splice(0).reverse())await close();});
/** Faults only the HTTP wire. Every successful request executes the production Hub. */
export async function proxyHub(target:string){
 const calls:JsonObject[]=[],held=new Map<string,(()=>void)[]>(),failures=new Map<string,number>(),statuses=new Map<string,number>(),blocked=new Set<string>();const abort=new AbortController();
 const server=createServer(async(req,res)=>{try{
 let raw="";for await(const bytes of req)raw+=String(bytes);const body=raw?object(JSON.parse(raw)):{};const op=String(body.op??req.url);calls.push({...body,path:req.url});
 if(blocked.has(op))await new Promise<void>(resolve=>{held.set(op,[...(held.get(op)??[]),resolve]);});
 const fail=failures.get(op)??0;if(fail>0){failures.set(op,fail-1);const status=statuses.get(op)??503;res.writeHead(status,{"content-type":"application/json"});res.end(JSON.stringify({ok:false,error:{code:status===401?"UNAUTHORIZED":"AUTH_UNAVAILABLE",message:"Controlled transport failure"}}));return;}
 const headers={...req.headers};delete headers.host;delete headers.connection;const result=await fetch(target+req.url,{method:req.method,headers:headers as Record<string,string>,body:req.method==="GET"?undefined:raw,signal:abort.signal});res.writeHead(result.status,Object.fromEntries(result.headers));if(result.body)await pipeline(Readable.fromWeb(result.body as never),res);else res.end();
 }catch(error){if(abort.signal.aborted||res.destroyed){res.destroy();return;}if(!res.headersSent)res.writeHead(502);res.end(String(error));}});
 await new Promise<void>(yes=>server.listen(0,"127.0.0.1",yes));const address=server.address();if(!address||typeof address==="string")throw new Error("No proxy port");
 let closed=false;const close=async()=>{if(closed)return;closed=true;abort.abort();for(const callbacks of held.values())for(const fn of callbacks)fn();server.closeAllConnections();await new Promise<void>(y=>server.close(()=>y()));};closers.push(close);
 return {url:`http://127.0.0.1:${address.port}`,calls,fail(op:string,count=1,status=503){failures.set(op,count);statuses.set(op,status);},hold(op:string){blocked.add(op);},release(op:string){blocked.delete(op);for(const fn of held.get(op)??[])fn();held.delete(op);},restore(){failures.clear();for(const op of blocked){blocked.delete(op);for(const fn of held.get(op)??[])fn();}held.clear();},close};
}
