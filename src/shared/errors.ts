export const ERROR_RETRYABLE = {
  BINDING_REQUIRED: false,
  SESSION_NOT_OPEN: false,
  PI_BRIDGE_UNAVAILABLE: true,
  INVALID_INPUT: false,
  UNSUPPORTED_HARNESS: false,
  UNAUTHORIZED: true,
  OIDC_LOGIN_TIMEOUT: true,
  AUTH_UNAVAILABLE: true,
  FORBIDDEN_TARGET_SCOPE: false,
  LISTENER_NOT_FOUND: false,
  LISTENER_EXPIRED: false,
  SELF_SEND_FORBIDDEN: false,
  IDEMPOTENCY_KEY_CONFLICT: false,
  STALE_GENERATION: true,
  NATS_UNAVAILABLE: true,
} as const;
export type ErrorCode = keyof typeof ERROR_RETRYABLE;

export class CitoError extends Error {
  readonly code: ErrorCode;
  readonly retryable: boolean;
  constructor(code: ErrorCode, message: string, options?: ErrorOptions) {
    super(message, options);
    this.code = code;
    this.name = "CitoError";
    this.retryable = ERROR_RETRYABLE[code];
  }
}

export interface McpResult {
  isError: boolean;
  content: { type: "text"; text: string }[];
}
export function successResult(payload: unknown): McpResult {
  const text = JSON.stringify(payload, null, 2);
  if (text === undefined) throw new TypeError("MCP result payload must be JSON serializable");
  return { isError: false, content: [{ type: "text", text }] };
}
export function errorResult(code: ErrorCode, message: string): McpResult {
  return {
    isError: true,
    content: [{ type: "text", text: JSON.stringify({
      ok: false, error: { code, message, retryable: ERROR_RETRYABLE[code] },
    }, null, 2) }],
  };
}
