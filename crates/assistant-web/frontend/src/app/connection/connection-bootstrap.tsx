import { useEffect, useState } from 'react';
import type { ReactNode } from 'react';
import * as v from 'valibot';
import { configureApi } from '@/shared/api';

const discoverySchema = v.object({ api_version: v.union([v.literal(1), v.literal(2)]) });

/** Select the local daemon's API, then open the conversation directly. */
export function ConnectionBootstrap({ children }: { children: ReactNode }) {
  const [ready, setReady] = useState(false);
  const [error, setError] = useState('');
  useEffect(() => {
    const controller = new AbortController();
    void fetch('/api/discovery', { signal: controller.signal, credentials: 'omit' })
      .then(async (reply) => {
        const version =
          reply.status === 404 ? 1 : v.parse(discoverySchema, await reply.json()).api_version;
        if (!reply.ok && reply.status !== 404) throw new Error('Assistant discovery failed');
        if (controller.signal.aborted) return;
        configureApi(version);
        setReady(true);
      })
      .catch(() => {
        if (!controller.signal.aborted) setError('Cannot connect to assistant. Reload to retry.');
      });
    return () => controller.abort();
  }, []);
  if (ready) return children;
  return (
    <main className="connection-screen">
      <p>{error || 'Connecting locally…'}</p>
    </main>
  );
}
