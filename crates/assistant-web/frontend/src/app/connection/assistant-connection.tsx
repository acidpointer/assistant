import { useEffect } from 'react';
import * as v from 'valibot';
import { request, response, readEvents, apiVersion } from '@/shared/api';
import {
  chatSnapshotSchema,
  chatUpdateSchema,
  replaceChat,
  replaceNotices,
  applyChatUpdate,
  connection,
} from '@/entities/chat';
import { resourceSchema, totalsSchema, updateTelemetry } from '@/entities/telemetry';
const bootstrapSchema = v.object({
  primary_chat_id: v.string(),
  limits: v.object({ deduplication_seconds: v.number() }),
});
const snapshotSchema = v.intersect([
  chatSnapshotSchema,
  v.object({ resources: v.nullable(resourceSchema), inference: totalsSchema }),
]);
const telemetrySchema = v.object({
  type: v.literal('telemetry'),
  sample: resourceSchema,
  inference: totalsSchema,
});

/** One stream per visible tab. Every reconnect starts with an authoritative snapshot.
 * Epoch checks prevent obsolete callbacks and late bootstraps from changing state. */
function startConnection() {
  let disposed = false;
  let epoch = 0;
  let source: EventSource | null = null;
  let controller: AbortController | null = null;
  let retry: ReturnType<typeof setTimeout> | undefined;
  let frame = 0;
  let pending: unknown[] = [];
  let delay = 500;
  function close() {
    epoch += 1;
    source?.close();
    source = null;
    controller?.abort();
    clearTimeout(retry);
    cancelAnimationFrame(frame);
    frame = 0;
    pending = [];
  }
  function recover() {
    close();
    connection('reconnecting');
    if (!disposed && !document.hidden) {
      retry = setTimeout(() => {
        void connect();
      }, delay);
      delay = Math.min(delay * 2, 10000);
    }
  }
  function flush() {
    frame = 0;
    const batch = pending;
    pending = [];
    try {
      for (const value of batch) {
        const telemetry = v.safeParse(telemetrySchema, value);
        if (telemetry.success) updateTelemetry(telemetry.output.sample, telemetry.output.inference);
        else applyChatUpdate(v.parse(chatUpdateSchema, value));
      }
    } catch {
      recover();
    }
  }
  async function connect() {
    close();
    if (disposed || document.hidden) return;
    connection('connecting');
    const current = epoch;
    controller = new AbortController();
    try {
      if (apiVersion() === 2) {
        const stream = await response('/events', { signal: controller.signal });
        if (disposed || current !== epoch || !stream.body) return;
        await readEvents(stream.body, controller.signal, (type, data) => {
          if (disposed || current !== epoch) return;
          const value: unknown = JSON.parse(data);
          if (type === 'snapshot') {
            const snapshot = v.parse(snapshotSchema, value);
            replaceChat(snapshot);
            updateTelemetry(snapshot.resources, snapshot.inference);
            delay = 500;
          } else if (type === 'notices') {
            replaceNotices(value);
          } else if (type === 'update') {
            const telemetry = v.safeParse(telemetrySchema, value);
            if (telemetry.success)
              updateTelemetry(telemetry.output.sample, telemetry.output.inference);
            else applyChatUpdate(v.parse(chatUpdateSchema, value));
          }
        });
        if (!disposed && current === epoch) recover();
        return;
      }
      const bootstrap = v.parse(
        bootstrapSchema,
        await request('/bootstrap', { signal: controller.signal }),
      );
      if (disposed || current !== epoch) return;
      source = new EventSource(
        `/api/v1/chats/${encodeURIComponent(bootstrap.primary_chat_id)}/events`,
      );
      source.addEventListener('snapshot', (event: MessageEvent<string>) => {
        if (current !== epoch) return;
        try {
          pending = [];
          cancelAnimationFrame(frame);
          frame = 0;
          const json: unknown = JSON.parse(event.data);
          const snapshot = v.parse(snapshotSchema, json);
          replaceChat(snapshot);
          updateTelemetry(snapshot.resources, snapshot.inference);
          delay = 500;
        } catch {
          recover();
        }
      });
      source.addEventListener('update', (event: MessageEvent<string>) => {
        if (current !== epoch) return;
        try {
          if (pending.length >= 256) {
            recover();
            return;
          }
          const value: unknown = JSON.parse(event.data);
          pending.push(value);
          if (!frame) frame = requestAnimationFrame(flush);
        } catch {
          recover();
        }
      });
      source.onerror = () => {
        if (current !== epoch) return;
        connection('reconnecting');
        if (source?.readyState === EventSource.CLOSED) recover();
      };
    } catch {
      if (current === epoch && !disposed) recover();
    }
  }
  const visibility = () => {
    if (document.hidden) {
      close();
      connection('suspended');
    } else void connect();
  };
  document.addEventListener('visibilitychange', visibility);
  void connect();
  return () => {
    disposed = true;
    close();
    document.removeEventListener('visibilitychange', visibility);
  };
}
export function AssistantConnection() {
  useEffect(startConnection, []);
  return null;
}
