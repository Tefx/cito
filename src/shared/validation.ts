import { z } from "zod";
import { CitoError } from "./errors.js";

export function sanitizeMessage(value: unknown): string {
  if (typeof value !== "string") throw new CitoError("INVALID_INPUT", "Expected a string");
  const cleaned = value.replace(/[\x00-\x08\x0b\x0c\x0e-\x1f\x7f]/g, "").trim();
  if (cleaned.length === 0 || Array.from(cleaned).length > 500) {
    throw new CitoError("INVALID_INPUT", "Text must contain 1-500 Unicode codepoints after sanitization");
  }
  return cleaned;
}
export function sanitizeSignalMessage(value: unknown): string {
  if (typeof value !== "string") throw new CitoError("INVALID_INPUT", "Expected a string");
  const cleaned = value.replace(/[\x00-\x08\x0b\x0c\x0e-\x1f\x7f]/g, "").trim();
  if (cleaned.length === 0 || Array.from(cleaned).length > 65_536) {
    throw new CitoError("INVALID_INPUT", "Signal message must contain 1-65536 Unicode codepoints after sanitization");
  }
  return cleaned;
}
export function escapeWakeCard(value: string): string {
  return value.replace(/</g, "&lt;").replace(/>/g, "&gt;");
}
export const harnessIdSchema = z.string().regex(/^[a-z][a-z0-9_-]{0,31}$/);
export type Harness = z.infer<typeof harnessIdSchema>;
export function validateHarness(value: unknown): Harness {
  const result = harnessIdSchema.safeParse(value);
  if (!result.success) throw new CitoError("UNSUPPORTED_HARNESS", "Invalid harness identifier");
  return result.data;
}
export const hubNameSchema = z.string().regex(/^[a-z0-9][a-z0-9_-]{0,31}$/);
export const canonicalListenerIdSchema = z.string().regex(/^c_[a-z2-7]{12}$/);
export const unscopedListenerIdSchema = z.string().regex(/^(?:c_[a-z2-7]{12}|[a-z0-9]+(?:-[a-z0-9]+)*-[a-z2-7]{12})$/);
export const listenerIdSchema = z.string().regex(/^(?:[a-z0-9][a-z0-9_-]{0,31}:)?(?:c_[a-z2-7]{12}|[a-z0-9]+(?:-[a-z0-9]+)*-[a-z2-7]{12})$/);
export const sendTargetSchema = z.string().regex(/^(?:[a-z0-9][a-z0-9_-]{0,31}:)?(?:c_[a-z2-7]{12}|[a-z0-9]+(?:-[a-z0-9]+)*-[a-z2-7]{12}|@[a-zA-Z0-9._-]{1,64})$/);
const textSchema = z.string().transform((value, ctx) => {
  try { return sanitizeMessage(value); }
  catch { ctx.addIssue({ code: "custom", message: "Expected 1-500 sanitized Unicode codepoints" }); return z.NEVER; }
});
const signalTextSchema = z.string().transform((value, ctx) => {
  try { return sanitizeSignalMessage(value); }
  catch { ctx.addIssue({ code: "custom", message: "Expected 1-65536 sanitized Unicode codepoints" }); return z.NEVER; }
});
export const listenInputSchema = z.object({
  about: textSchema, tag: z.string().max(64).optional(), hub: hubNameSchema.optional(),
  scope: z.enum(["self", "workspace"]).optional(),
}).strict();
export const unlistenInputSchema = z.object({ hub: hubNameSchema.optional() }).strict();
export const findInputSchema = z.object({
  query: z.string().max(200).optional(), hub: hubNameSchema.optional(), user: z.string().max(64).optional(),
  host: z.string().max(128).optional(), harness: harnessIdSchema.optional(), repo: z.string().max(200).optional(),
  limit: z.number().int().min(1).max(50).default(20),
}).strict();
export const sendInputSchema = z.object({
  to: sendTargetSchema, hub: hubNameSchema.optional(), message: signalTextSchema,
  in_reply_to: z.string().min(1).max(161).optional(), idempotency_key: z.string().min(1).max(128).optional(),
}).strict();
export const inboxInputSchema = z.object({
  hub: hubNameSchema.optional(), ack_ids: z.array(z.string().min(1).max(161)).optional(), ack_all: z.boolean().optional(),
}).strict();
export function parseInput<T>(schema: z.ZodType<T>, input: unknown): T {
  const result = schema.safeParse(input);
  if (!result.success) {
    if (result.error.issues.some(issue => issue.path[0] === "harness")) {
      throw new CitoError("UNSUPPORTED_HARNESS", "Invalid harness identifier");
    }
    throw new CitoError("INVALID_INPUT", result.error.message);
  }
  return result.data;
}
