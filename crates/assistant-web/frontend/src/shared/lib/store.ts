/** Keyed notifications let streaming text update one row without waking the page. */
export function createNotifications() {
  const listeners = new Map<string, Set<() => void>>();
  return {
    subscribe(key: string, listener: () => void) {
      const group = listeners.get(key) ?? new Set<() => void>();
      group.add(listener);
      listeners.set(key, group);
      return () => {
        group.delete(listener);
        if (!group.size) listeners.delete(key);
      };
    },
    notify(key: string) {
      listeners.get(key)?.forEach((listener) => listener());
    },
  };
}
