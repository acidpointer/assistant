import { useSyncExternalStore } from 'react';
import * as v from 'valibot';
import { createNotifications } from '@/shared/lib/store';

const noticeSchema = v.object({
  id: v.string(),
  text: v.pipe(v.string(), v.maxLength(4096)),
  created_at: v.number(),
  dismissed: v.boolean(),
});
const noticesSchema = v.pipe(v.array(noticeSchema), v.maxLength(32));
const notifications = createNotifications();
let notices: readonly v.InferOutput<typeof noticeSchema>[] = [];
export function replaceNotices(value: unknown) {
  notices = v.parse(noticesSchema, value);
  notifications.notify('notices');
}
export function clearNotices() {
  notices = [];
  notifications.notify('notices');
}
const subscribe = (listener: () => void) => notifications.subscribe('notices', listener);
const snapshot = () => notices;
export function useNotices() {
  return useSyncExternalStore(subscribe, snapshot);
}
