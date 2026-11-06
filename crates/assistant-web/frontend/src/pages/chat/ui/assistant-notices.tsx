import { useEffect, useRef, useState } from 'react';
import { useNotices, replaceNotices } from '@/entities/chat';
import { response, request } from '@/shared/api';
import { Button } from '@/shared/ui/button';

/** Durable assistant messages arrive even when no user run is active. */
export function AssistantNotices() {
  const notices = useNotices();
  const mounted = useRef(true);
  useEffect(
    () => () => {
      mounted.current = false;
    },
    [],
  );
  const [error, setError] = useState('');
  const [busy, setBusy] = useState<string | null>(null);
  const visible = notices.filter((notice) => !notice.dismissed).reverse();
  if (!visible.length) return null;
  return (
    <section
      className="assistant-notices"
      aria-label="Assistant started a conversation"
      aria-live="polite"
    >
      {visible.map((notice) => (
        <article className="assistant-notice" key={notice.id}>
          <div className="memory-heading">
            <span>✦ assistant · {new Date(notice.created_at).toLocaleString()}</span>
            <Button
              disabled={busy !== null}
              onClick={() => {
                setBusy(notice.id);
                setError('');
                void response(`/assistant/notices/${encodeURIComponent(notice.id)}/dismiss`, {
                  method: 'POST',
                  body: '{}',
                })
                  .then(() => request('/assistant/notices'))
                  .then((value) => {
                    if (mounted.current) replaceNotices(value);
                  })
                  .catch(() => setError('Could not dismiss this message.'))
                  .finally(() => setBusy(null));
              }}
            >
              Dismiss
            </Button>
          </div>
          <p>{notice.text}</p>
        </article>
      ))}
      {error && <p role="alert">{error}</p>}
    </section>
  );
}
