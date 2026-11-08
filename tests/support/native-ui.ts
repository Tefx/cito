import { copyFile, mkdir, stat, readdir, readFile } from "node:fs/promises";
import { join } from "node:path";
import { createInterface } from "node:readline";
import { NativeHarness, object, projectRoot, type NativeProcess } from "./native-harness.js";
type AdapterTier="stable"|"experimental"|"custom";
type HubAuthMode="none"|"token"|"oidc";
export interface UiHarnessMetadata {id:string;display_name:string;sf_symbol:string;tint:string;tier:AdapterTier;order?:number}
export interface AvailableSessionSummary {harness:string;session_id:string;cwd:string;title:string;repo?:string|null;branch?:string|null;endpoint_pid?:number|null;updated_at_ms?:number;listening_hubs?:string[]}
interface UiChannelFieldSummary {key:string;scope:"hub"|"user";label:string;type:string;required:boolean;placeholder:string;value:string;configured:boolean;hidden?:boolean}
interface UiChannelSetupSummary {id:string;display_name:string;sf_symbol:string;tint:string;enabled:boolean;configured:boolean;status:string;detail:string;display_label?:string|null;hub_provided:boolean;pairing_supported:boolean;pairing_action_label?:string|null;pairing_code?:string|null;pairing_instruction?:string|null;fields:UiChannelFieldSummary[]}
export interface BannerPayload {
  target_harness: string;
  target_session_id: string;
  target_title: string;
  from_user: string;
  from_host: string;
  from_title: string;
  message: string;
  effective_wake: boolean;
  count?: number;
  target_cwd?: string | null;
  signal_id?: string | null;
  hub?: string | null;
  from_scoped_id?: string | null;
}
export interface UiHubSummary {
  name: string;
  status: "connected" | "connecting" | "auth_required" | "disconnected";
  user: string | null;
  auth_mode: "none" | "token" | "oidc";
  error: string | null;
}
export interface UiListenerSummary {
  hub: string;
  listener_id: string;
  public_id: string;
  scoped_id: string;
  harness: string;
  session_id: string;
  title: string;
  about?: string;
  cwd: string;
  repo: string | null;
  branch?: string | null;
  scope: "self" | "workspace";
  channels?: string[];
}

export interface UiRemotePeerSummary {
  hub: string;
  listener_id: string;
  public_id: string;
  scoped_id: string;
  user: string;
  host_id: string;
  host_name: string;
  harness: string;
  title: string;
  about: string;
  cwd?: string;
  repo: string | null;
  branch?: string | null;
  scope: "self" | "workspace";
  last_seen_at: string;
  is_self_principal?: boolean;
  channels?: string[];
}
export interface UiActivityItem {
  hub: string;
  signal_id: string;
  listener_id: string;
  from_user: string;
  from_host_name: string;
  from_harness: string | null;
  from_title: string;
  from_scoped_id: string | null;
  target_harness: string;
  target_title: string;
  target_scoped_id: string;
  message: string;
  held_reason: string | null;
  in_reply_to: string | null;
  created_at: string;
  unread: boolean;
  for_me: boolean;
}
export interface UiAppSetupSummary {
  id: string;
  display_name: string;
  tier: AdapterTier;
  installed: boolean;
  configured: boolean;
  status: "ready" | "not_configured" | "conflict";
  detail: string;
  paths: string[];
}
export interface UiHostHubSummary {
  enabled: boolean;
  running: boolean;
  port: number;
  endpoint_url: string;
  token: string;
  invite_command: string;
  error: string | null;
}
export interface UiHubConfigSummary {
  name: string;
  url: string;
  auth_mode: HubAuthMode;
  enabled: boolean;
  is_default: boolean;
  status: "connected" | "connecting" | "auth_required" | "disconnected";
  user: string | null;
  error: string | null;
}
export interface UiSettingsState {
  apps: UiAppSetupSummary[];
  channels?: UiChannelSetupSummary[];
  desktop_banner_enabled?: boolean;
  hubs: UiHubConfigSummary[];
  host_hub: UiHostHubSummary;
  launch_at_login: boolean;
}
export type DaemonToUiMessage =
  | {
      op: "state";
      hubs: UiHubSummary[];
      listeners: UiListenerSummary[];
      available?: AvailableSessionSummary[];
      my_remote_listeners?: UiRemotePeerSummary[];
      directory_online_count?: number;
      unread_count?: number;
      activity?: UiActivityItem[];
      harnesses?: UiHarnessMetadata[];
      settings?: UiSettingsState;
    }
  | {
      op: "directory";
      peers: UiRemotePeerSummary[];
      matched_count: number;
      online_count: number;
      activity?: UiActivityItem[];
      unread_count?: number;
    }
  | { op: "banner"; payload: BannerPayload };
export type UiToDaemonAction =
  | { op: "unlisten"; hub: string; listener_id: string }
  | { op: "unlisten_all" }
  | { op: "set_scope"; hub: string; listener_id: string; scope: "self" | "workspace" }
  | { op: "hub_action"; hub: string; action: "login" | "reconnect" | "disconnect" }
  | { op: "refresh_available" }
  | { op: "listen_session"; harness: string; session_id: string; endpoint_pid?: number; cwd?: string; title?: string; hub?: string; scope?: "self" | "workspace" }
  | { op: "switch_hub"; from_hub: string; to_hub: string; listener_id: string }
  | { op: "open_directory"; mode?: "directory" | "activity" }
  | { op: "close_directory" }
  | { op: "query_directory"; query?: string; scope?: "all" | "mine"; harness?: string; mode?: "directory" | "activity"; filter?: "me" | "all" | "unread" }
  | { op: "send_signal"; hub?: string; to: string; message: string; in_reply_to?: string | null }
  | { op: "ack_signals"; hub?: string; signal_ids?: string[]; ack_all?: boolean }
  | { op: "open_settings"; hub?: string }
  | { op: "close_settings" }
  | { op: "setup_apply"; harness: string; force?: boolean }
  | { op: "setup_remove_app"; harness: string }
  | { op: "setup_uninstall_all"; purge_data?: boolean }
  | { op: "set_host_hub"; enabled: boolean; port?: number; reset_token?: boolean }
  | { op: "set_launch_at_login"; enabled: boolean }
  | { op: "set_desktop_banner"; enabled: boolean }
  | { op: "channel_save"; channel_id: string; hub?: string; enabled?: boolean; values: Record<string, string> }
  | { op: "channel_test"; channel_id: string; hub?: string; values?: Record<string, string> }
  | { op: "channel_pair"; channel_id: string; hub?: string; values?: Record<string, string> }
  | { op: "setup_save_hub"; name: string; url: string; auth_mode: HubAuthMode; token?: string; issuer?: string; client_id?: string; user?: string; set_default?: boolean }
  | { op: "setup_toggle_hub"; hub: string; enabled: boolean }
  | { op: "setup_remove_hub"; hub: string }
  | { op: "quit" };
export interface UiOptions {
  env?: NodeJS.ProcessEnv;
  sourcePath?: string;
  binary?: string;
  diagnostic?: (message: string) => void;
  onAction?: (action: UiToDaemonAction) => void | Promise<void>;
}

export async function ensureUiCompiled(stateDir:string,source?:string):Promise<string>{
 if(source)throw new Error("Custom source compilation belongs to the native build, not the test transport");
 const dest=join(stateDir,"bin/cito-ui"),src=join(projectRoot,"bin/cito-ui-darwin-arm64");
 if(await stat(dest).then(()=>true,()=>false))return dest;
 await mkdir(join(stateDir,"bin"),{recursive:true});await copyFile(src,dest);return dest;
}
/** Raw test transport: production Swift owns rendering, parsing, actions, and frame limits. */
export class UiBridge{
 private child?: NativeProcess["child"];private process?:NativeProcess;private harness?:NativeHarness;private defaults:UiHarnessMetadata[]=[];
 readonly stateDir:string;private options:UiOptions;
 constructor(stateDir:string,options:UiOptions={}){this.stateDir=stateDir;this.options=options;}
 async start(){for(const entry of await readdir(join(projectRoot,"plugins"),{withFileTypes:true})){if(!entry.isDirectory())continue;const path=join(projectRoot,"plugins",entry.name,"plugin.json");if(!await stat(path).then(()=>true,()=>false))continue;const m=object(JSON.parse(await readFile(path,"utf8")));if(m.kind!=="agent-harness")continue;if(typeof m.id!=="string"||typeof m.displayName!=="string")throw new Error("Invalid harness metadata");this.defaults.push({id:m.id,display_name:m.displayName,sf_symbol:String(m.sfSymbol??"terminal"),tint:String(m.tint??"secondary"),tier:m.tier==="experimental"?"experimental":"stable",order:this.defaults.length});}this.harness=await NativeHarness.create();this.harness.env.CITO_STATE_DIR=this.stateDir;this.harness.env.CITO_CONFIG_PATH=join(this.stateDir,"config.json");try{this.process=await this.harness.spawn(this.options.binary??"bin/cito-ui-darwin-arm64",[],{env:this.options.env});this.child=this.process.child;}catch(error){this.options.diagnostic?.(String(error));return;}
 createInterface({input:this.child.stderr}).on("line",line=>this.options.diagnostic?.(line));
 createInterface({input:this.child.stdout}).on("line",line=>{try{const value=JSON.parse(line);void this.options.onAction?.(value);}catch{this.options.diagnostic?.("Invalid UI JSON");}});
 this.state([],[]);}
 private send(frame:unknown){this.child?.stdin.write(JSON.stringify(frame)+"\n");}
 state(hubs:UiHubSummary[],listeners:UiListenerSummary[],available:AvailableSessionSummary[]=[],myRemote:UiRemotePeerSummary[]=[],count=0,harnesses:UiHarnessMetadata[]|undefined=undefined,settings?:UiSettingsState,unread=0,activity:UiActivityItem[]=[]){this.send({op:"state",hubs,listeners,available,my_remote_listeners:myRemote,directory_online_count:count,harnesses:harnesses??this.defaults,settings,unread_count:unread,activity});}
 directory(peers:UiRemotePeerSummary[],matched=peers.length,online=peers.length,activity:UiActivityItem[]=[],unread=0){this.send({op:"directory",peers,matched_count:matched,online_count:online,activity,unread_count:unread});}
 banner(payload:BannerPayload){this.send({op:"banner",payload});}
 async close(){if(this.child){this.child.stdin.end();await Promise.race([this.process?.result,new Promise(r=>setTimeout(r,1000))]);}await this.harness?.close();}
}
