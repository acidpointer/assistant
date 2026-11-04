import { useSyncExternalStore } from 'react';
import type { ResourceSample, Totals } from './contracts';
import { createNotifications } from '@/shared/lib/store';
const notifications = createNotifications();
let state: { sample: ResourceSample | null; totals: Totals } = {
  sample: null,
  totals: { generated_tokens: 0, generation_ns: 0 },
};
const subscribe = (listener: () => void) => notifications.subscribe('telemetry', listener);
export function updateTelemetry(sample: ResourceSample | null, totals: Totals) {
  state = { sample, totals };
  notifications.notify('telemetry');
}
export function useTelemetry() {
  return useSyncExternalStore(subscribe, () => state);
}
export function tokensPerSecond(tokens: number, nanoseconds: number): number | null {
  return nanoseconds > 0 ? (tokens * 1e9) / nanoseconds : null;
}
export function memory(bytes: number | null | undefined): string {
  if (bytes == null) return 'Unavailable';
  if (bytes >= 1024 ** 3) return `${(bytes / 1024 ** 3).toFixed(2)} GiB`;
  return `${(bytes / 1024 ** 2).toFixed(1)} MiB`;
}
