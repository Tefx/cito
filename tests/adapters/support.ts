import {NativeHarness} from "../support/native-harness.js";
export {socketServer as unixServer} from "../support/native-scenarios.js";
export async function isolatedState(){const harness=await NativeHarness.create();return {harness,root:harness.root,stateDir:harness.stateDir,codexHome:harness.env.CITO_CODEX_HOME!,claudeHome:harness.env.CITO_CLAUDE_HOME!,env:harness.env,cleanup:()=>harness.close()};}
