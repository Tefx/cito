import { Server } from "@modelcontextprotocol/sdk/server/index.js";
import { CallToolRequestSchema, ListToolsRequestSchema, type Tool } from "@modelcontextprotocol/sdk/types.js";
import { CitoService } from "./service.js";

const object = (properties: Record<string, object>, required: string[] = []): Tool["inputSchema"] =>
  ({ type: "object", properties, required, additionalProperties: false });
const text = { type: "string", minLength: 1, maxLength: 500 };
export const tools: Tool[] = [
  { name: "cito_listen", description: "Enroll this Codex or Pi session to receive notifications.", inputSchema: object({
    about: { ...text, description: "Concise session responsibility." },
    tag: { type: "string", maxLength: 64, description: "Optional short display name; preserved on later enrollments." }, scope: { type: "string", enum: ["self", "workspace"] },
    allow_peer_wake: { type: "boolean" } }, ["about"]) },
  { name: "cito_unlisten", description: "Revoke this session's registration and cancel pending wakes.", inputSchema: object({}) },
  { name: "cito_find", description: "Find active opted-in sessions. Caller binding is optional.", inputSchema: object({
    query: { type: "string", maxLength: 200 }, user: { type: "string", maxLength: 64 },
    host: { type: "string", maxLength: 128 }, harness: { type: "string", enum: ["codex", "pi"] },
    repo: { type: "string", maxLength: 200 }, limit: { type: "integer", minimum: 1, maximum: 50, default: 20 } }) },
  { name: "cito_send", description: "Notify an exact listener ID. Caller binding is optional.", inputSchema: object({
    to: { type: "string", pattern: "^(?:c_[a-z2-7]{12}|[a-z0-9]+(?:-[a-z0-9]+)*-[a-z2-7]{12})$" }, message: text, wake: { type: "boolean", default: true },
    in_reply_to: { type: "string", maxLength: 64 }, idempotency_key: { type: "string", minLength: 1, maxLength: 128 } }, ["to", "message"]) },
  { name: "cito_inbox", description: "Read and acknowledge this session's local inbox, including offline.", inputSchema: object({
    ack_ids: { type: "array", items: { type: "string" } }, ack_all: { type: "boolean", default: false } }) },
];

export function createMcpServer(service: CitoService): Server {
  const server = new Server({ name: "cito-mcp", version: "0.1.0" }, { capabilities: { tools: {} } });
  server.setRequestHandler(ListToolsRequestSchema, async () => ({ tools }));
  server.setRequestHandler(CallToolRequestSchema, async request => {
    const result = await service.call(request.params.name, request.params.arguments ?? {}, request.params._meta ?? {});
    return { ...result };
  });
  return server;
}
