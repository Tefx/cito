import {NativeHarness,type JsonObject} from "./native-harness.js";
import {request} from "./plugin-fixtures.js";
export const bridgeSocketPath=(state:string,harness:string,pid:number)=>`${state}/sockets/${harness}-pid-${pid}.sock`;
export async function requestBridge(path:string,value:object,timeout=1500){const r=await request(path,value as JsonObject,{timeout});if(r.ok===false)throw Object.assign(new Error(String(r.message??r.error)),{bridgeError:r.error,code:"PI_BRIDGE_UNAVAILABLE"});return r;}
export const identifyBridge=(id:string,pid:number,state:{stateDir:string})=>requestBridge(bridgeSocketPath(state.stateDir,id,pid),{op:"identify"});
export interface DeliverContext {batchId:string;card:string;config:NativeHarness;signal:AbortSignal;queueWritten:boolean;markQueueWritten():void;runEffect(submit:()=>Promise<void>):Promise<boolean>}
export class SocketBridgeAdapter{
 private id:string;constructor(id:string){this.id=id;}
 async updateBadge(row:JsonObject,badge:string|undefined,f:NativeHarness){const r=await f.plugin(this.id,{op:"update_badge",session_id:row.session_id,endpoint_pid:row.endpoint_pid,badge:badge??null});if(r.ok===false)throw Object.assign(new Error(String(r.message??r.error)),{bridgeError:r.error});}
 async deliver(row:JsonObject,context:DeliverContext){const r=await context.config.plugin(this.id,{op:"wake",session_id:row.session_id,endpoint_pid:row.endpoint_pid,batch_id:context.batchId,text:context.card,wake:true,signals:[]});return r.outcome;}
}
