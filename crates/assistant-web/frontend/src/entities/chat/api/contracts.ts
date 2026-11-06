import * as v from 'valibot';
import { performanceSchema, windowSchema } from '@/entities/telemetry/@x/chat';
const id = v.pipe(v.string(), v.minLength(1), v.maxLength(256));
const statusSchema = v.picklist([
  'accepted',
  'running',
  'stopping',
  'completed',
  'truncated',
  'cancelled',
  'deadline',
  'failed',
]);
export const messageSchema = v.object({
  id,
  run_id: id,
  role: v.picklist(['user', 'assistant']),
  text: v.pipe(v.string(), v.maxLength(131072)),
  status: statusSchema,
});
export const runSchema = v.object({
  id,
  chat_id: id,
  client_identity_available: v.optional(v.boolean()),
  usage_available: v.optional(v.boolean()),
  outcome_details_available: v.optional(v.boolean()),
  client_id: v.pipe(v.string(), v.maxLength(128)),
  client_request_id: v.pipe(v.string(), v.maxLength(128)),
  status: statusSchema,
  error: v.nullable(v.string()),
  input_tokens: v.number(),
  generated_tokens: v.number(),
  warning_count: v.number(),
  unknown_tool_outcome: v.boolean(),
  limit_reason: v.nullable(v.string()),
  performance: v.pipe(v.array(performanceSchema), v.maxLength(8)),
  resources: windowSchema,
});
export const chatSnapshotSchema = v.object({
  instance_id: id,
  chat_id: id,
  revision: id,
  messages: v.pipe(v.array(messageSchema), v.maxLength(128)),
  active_run: v.nullable(runSchema),
  recent_runs: v.pipe(v.array(runSchema), v.maxLength(128)),
  truncated: v.boolean(),
});
export const chatUpdateSchema = v.variant('type', [
  v.object({ type: v.literal('run'), run: runSchema }),
  v.object({ type: v.literal('message'), message: messageSchema }),
  v.object({
    type: v.literal('delta'),
    message_id: id,
    text: v.pipe(v.string(), v.maxLength(16384)),
  }),
  v.object({ type: v.literal('activity'), run_id: id, label: v.string() }),
  v.object({ type: v.literal('evicted'), message_ids: v.array(id) }),
]);
export type Message = v.InferOutput<typeof messageSchema>;
export type Run = v.InferOutput<typeof runSchema>;
export type ChatSnapshot = v.InferOutput<typeof chatSnapshotSchema>;
export type ChatUpdate = v.InferOutput<typeof chatUpdateSchema>;
export function parseRun(value: unknown): Run {
  return v.parse(runSchema, value);
}
export function terminal(status: Run['status']): boolean {
  return !['accepted', 'running', 'stopping'].includes(status);
}
