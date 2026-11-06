import { useCallback, useSyncExternalStore } from 'react';
import { createNotifications } from '@/shared/lib/store';
import type { ChatSnapshot, ChatUpdate, Message, Run } from '../api/contracts';
import { clearNotices } from './notices';
import { terminal } from '../api/contracts';
const notifications = createNotifications();
const messages = new Map<string, Message>();
const messageBytes = new Map<string, number>();
const encoder = new TextEncoder();
const runs = new Map<string, Run>();
let ids: readonly string[] = [];
let meta = {
  chatId: '',
  instanceId: '',
  activeRunId: null as string | null,
  connection: 'connecting',
  activity: '',
  truncated: false,
};
function metadata(patch: Partial<typeof meta>) {
  const next = { ...meta, ...patch };
  // Keys belong to this closed, host-owned object; incoming JSON never enters it.
  const keys = Object.keys(meta) as (keyof typeof meta)[];
  if (keys.every((key) => next[key] === meta[key])) return;
  meta = next;
  notifications.notify('meta');
}
export function connection(status: string) {
  metadata({ connection: status });
}
export function getChat() {
  return meta;
}
export function clearChat() {
  clearNotices();
  const oldIds = ids;
  const oldRuns = Array.from(runs.keys());
  messages.clear();
  messageBytes.clear();
  runs.clear();
  ids = [];
  metadata({
    chatId: '',
    instanceId: '',
    activeRunId: null,
    connection: 'locked',
    activity: '',
    truncated: false,
  });
  notifications.notify('ids');
  oldIds.forEach((id) => notifications.notify(id));
  oldRuns.forEach((id) => notifications.notify(`run:${id}`));
}
export function getRun(id: string) {
  return runs.get(id);
}
export function findRequest(client: string, request: string) {
  return Array.from(runs.values()).find(
    (run) => run.client_id === client && run.client_request_id === request,
  );
}
export function replaceChat(snapshot: ChatSnapshot) {
  const oldIds = ids;
  const oldRuns = Array.from(runs.keys());
  const sizes = new Map<string, number>();
  let bytes = 0;
  for (const message of snapshot.messages) {
    const size = encoder.encode(message.text).length;
    if (sizes.has(message.id) || size > 131072) throw new Error('Invalid snapshot message');
    sizes.set(message.id, size);
    bytes += size;
  }
  if (bytes > 1048576) throw new Error('Snapshot text limit');
  messages.clear();
  messageBytes.clear();
  runs.clear();
  snapshot.messages.forEach((message) => {
    messages.set(message.id, message);
    messageBytes.set(message.id, sizes.get(message.id) ?? 0);
  });
  snapshot.recent_runs.forEach((run) => runs.set(run.id, run));
  if (snapshot.active_run) runs.set(snapshot.active_run.id, snapshot.active_run);
  ids = snapshot.messages.map((message) => message.id);
  metadata({
    chatId: snapshot.chat_id,
    instanceId: snapshot.instance_id,
    activeRunId: snapshot.active_run?.id ?? null,
    truncated: snapshot.truncated,
    connection: 'online',
    activity: snapshot.active_run ? 'Thinking' : '',
  });
  notifications.notify('ids');
  new Set([...oldIds, ...ids]).forEach((id) => notifications.notify(id));
  new Set([...oldRuns, ...runs.keys()]).forEach((id) => notifications.notify(`run:${id}`));
}
export function applyChatUpdate(update: ChatUpdate) {
  switch (update.type) {
    case 'message': {
      const size = encoder.encode(update.message.text).length;
      if (size > 131072) throw new Error('Message text limit');
      messageBytes.set(update.message.id, size);
      if (!messages.has(update.message.id)) {
        ids = [...ids, update.message.id];
        notifications.notify('ids');
      }
      messages.set(update.message.id, update.message);
      notifications.notify(update.message.id);
      break;
    }
    case 'delta': {
      const previous = messages.get(update.message_id);
      // Re-encoding the growing answer per delta would make streaming quadratic.
      const size = (messageBytes.get(update.message_id) ?? 0) + encoder.encode(update.text).length;
      if (!previous || size > 131072) {
        throw new Error('Invalid message delta; snapshot required');
      }
      messageBytes.set(previous.id, size);
      messages.set(previous.id, { ...previous, text: previous.text + update.text });
      notifications.notify(previous.id);
      break;
    }
    case 'run': {
      const isNew = !runs.has(update.run.id);
      runs.set(update.run.id, update.run);
      if (isNew && runs.size > 256) {
        const visibleRuns = new Set(Array.from(messages.values(), (message) => message.run_id));
        for (const [id, run] of runs) {
          if (runs.size <= 128) break;
          if (terminal(run.status) && !visibleRuns.has(id)) runs.delete(id);
        }
      }
      notifications.notify(`run:${update.run.id}`);
      if (!terminal(update.run.status)) metadata({ activeRunId: update.run.id });
      else if (meta.activeRunId === update.run.id) metadata({ activeRunId: null, activity: '' });
      break;
    }
    case 'activity':
      metadata({ activity: update.label });
      break;
    case 'evicted': {
      const removed = new Set(update.message_ids);
      removed.forEach((id) => {
        messages.delete(id);
        messageBytes.delete(id);
      });
      ids = ids.filter((id) => !removed.has(id));
      metadata({ truncated: true });
      notifications.notify('ids');
      break;
    }
  }
}
function useKey<T>(key: string, read: () => T): T {
  return useSyncExternalStore(
    useCallback((listener) => notifications.subscribe(key, listener), [key]),
    read,
  );
}
export function useChat() {
  return useKey('meta', () => meta);
}
export function useMessageIds() {
  return useKey('ids', () => ids);
}
export function useMessage(id: string) {
  return useKey(id, () => messages.get(id));
}
export function useRun(id: string | null) {
  return useKey(`run:${id ?? ''}`, () => (id ? runs.get(id) : undefined));
}
