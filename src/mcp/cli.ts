import { StdioServerTransport } from "@modelcontextprotocol/sdk/server/stdio.js";
import { loadConfig } from "../shared/index.js";
import { CitoService } from "./service.js";
import { createMcpServer } from "./server.js";

export async function main(): Promise<void> {
  const config = loadConfig();
  const service = await CitoService.create(config);
  const server = createMcpServer(service);
  let stopping: Promise<void> | undefined;
  const stop = () => stopping ??= (async () => { await service.close(); await server.close(); })();
  const report = (error: unknown) => { console.error(error); process.exitCode = 1; };
  const onSignal = () => { void stop().catch(report); };
  process.once("SIGINT", onSignal); process.once("SIGTERM", onSignal);
  process.stdin.once("end", onSignal);
  server.onclose = () => { void stop().catch(report).finally(() => {
    process.removeListener("SIGINT", onSignal); process.removeListener("SIGTERM", onSignal);
    process.stdin.removeListener("end", onSignal);
  }); };
  try { await server.connect(new StdioServerTransport()); }
  catch (error) { await stop(); throw error; }
}
