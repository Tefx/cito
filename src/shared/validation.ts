import { z } from "zod";
import { CitoError } from "./errors.js";

export function sanitizeMessage(value: unknown): string {
  if (typeof value !== "string") throw new CitoError("INVALID_INPUT", "Expected a string");
  const cleaned = value.replace(/[\x00-\x08\x0b\x0c\x0e-\x1f\x7f]/g, "").trim();
  if (cleaned.length === 0 || Array.from(cleaned).length > 500) {
    throw new CitoError("INVALID_INPUT", "Text must contain 1–500 Unicode codepoints after sanitization");
  }
  return cleaned;
}

/** Escape only at rendering time; stored messages remain sanitized plain text. */
export function escapeWakeCard(value: string): string {
  return value.replace(/</g, "&lt;").replace(/>/g, "&gt;");
}

export const harnessSchema = z.enum(["codex", "pi"]);
export type Harness = z.infer<typeof harnessSchema>;
export function validateHarness(value: unknown): Harness {
  const result = harnessSchema.safeParse(value);
  if (!result.success) throw new CitoError("UNSUPPORTED_HARNESS", "V1 supports only codex and pi");
  return result.data;
}
export const listenerIdSchema = z.string().regex(/^c_[a-z2-7]{12}$/);
// Refine by codepoints after cleaning: z.string().max(500) would reject valid emoji.
const textSchema = z.unknown().transform((value, ctx) => {
  try { return sanitizeMessage(value); }
  catch {
    ctx.addIssue({ code: "custom", message: "Expected 1–500 sanitized Unicode codepoints" });
    return z.NEVER;
  }
});
export const listenInputSchema = z.object({
  about: textSchema,
  scope: z.enum(["self", "workspace"]).optional(),
  allow_peer_wake: z.boolean().optional(),
}).strict();
export const unlistenInputSchema = z.object({}).strict();
export const findInputSchema = z.object({
  query: z.string().max(200).optional(),
  user: z.string().max(64).optional(),
  host: z.string().max(128).optional(),
  harness: harnessSchema.optional(),
  repo: z.string().max(200).optional(),
  limit: z.number().int().min(1).max(50).default(20),
}).strict();
export const sendInputSchema = z.object({
  to: listenerIdSchema,
  message: textSchema,
  wake: z.boolean().default(true),
  in_reply_to: z.string().max(64).optional(),
  idempotency_key: z.string().min(1).max(128).optional(),
}).strict();
export const inboxInputSchema = z.object({
  ack_ids: z.array(z.string()).optional(),
  ack_all: z.boolean().default(false),
}).strict();

export function parseInput<T>(schema: z.ZodType<T>, input: unknown): T {
  const result = schema.safeParse(input);
  if (!result.success) {
    if (result.error.issues.some(issue => issue.path[0] === "harness")) {
      throw new CitoError("UNSUPPORTED_HARNESS", "V1 supports only codex and pi");
    }
    throw new CitoError("INVALID_INPUT", result.error.message);
  }
  return result.data;
}
