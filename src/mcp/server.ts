import { Server } from "@modelcontextprotocol/sdk/server/index.js";
import { CallToolRequestSchema, ListToolsRequestSchema, type Tool } from "@modelcontextprotocol/sdk/types.js";
import { z } from "zod";
import { listenInputSchema, unlistenInputSchema, findInputSchema, sendInputSchema, inboxInputSchema } from "../shared/index.js";
import { CitoService } from "./service.js";

// Derive the advertised schema from the same validation contract used by service calls.
const descriptors = [
  ["cito_listen", "Enroll this session to receive notifications.", listenInputSchema],
  ["cito_unlisten", "Revoke this session on one hub or every active hub.", unlistenInputSchema],
  ["cito_find", "Find active opted-in sessions across configured hubs. Caller binding is optional.", findInputSchema],
  ["cito_send", "Notify an exact listener, optionally prefixed with its hub. Caller binding is optional.", sendInputSchema],
  ["cito_inbox", "Read and drain this session's local inbox by default, including offline. Set ack_all:false to peek.", inboxInputSchema],
] as const;
export const tools: Tool[] = descriptors.map(([name, description, schema]) => ({
  name, description, inputSchema: z.toJSONSchema(schema, { io: "input", unrepresentable: "any" }) as Tool["inputSchema"],
}));
export function createMcpServer(service: CitoService): Server {
  const server = new Server({ name: "cito-mcp", version: "0.1.0" }, { capabilities: { tools: {} } });
  server.setRequestHandler(ListToolsRequestSchema, async () => ({ tools }));
  server.setRequestHandler(CallToolRequestSchema, async request => {
    const result = await service.call(request.params.name, request.params.arguments ?? {}, request.params._meta ?? {});
    return { ...result };
  });
  return server;
}
