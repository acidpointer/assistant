import { useEffect, useState } from 'react';
import * as v from 'valibot';
import { request } from '@/shared/api';
import { Button } from '@/shared/ui/button';

const source = v.object({
  source: v.object({ id: v.string(), text: v.pipe(v.string(), v.maxLength(131072)) }),
  role: v.picklist(['user', 'assistant', 'tool']),
  observed_at: v.nullable(v.number()),
});
const history = v.object({
  exchanges: v.pipe(
    v.array(
      v.object({
        input: source,
        assistant: v.nullable(source),
        operation: v.object({ id: v.string(), state: v.string() }),
      }),
    ),
    v.maxLength(16),
  ),
  next_before: v.nullable(v.string()),
});
type History = v.InferOutput<typeof history>;

/** One bounded durable page, independent of the changing live transcript. */
export function SavedHistory() {
  const [open, setOpen] = useState(false);
  const [before, setBefore] = useState<string | null>(null);
  const [page, setPage] = useState<History | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState('');
  useEffect(() => {
    if (!open) return;
    const controller = new AbortController();
    void request(`/history?limit=16${before ? `&before=${encodeURIComponent(before)}` : ''}`, {
      signal: controller.signal,
    })
      .then((value) => setPage(v.parse(history, value)))
      .catch(() => {
        if (!controller.signal.aborted) setError('Saved history is unavailable.');
      })
      .finally(() => {
        if (!controller.signal.aborted) setBusy(false);
      });
    return () => controller.abort();
  }, [open, before]);
  return (
    <section aria-label="Saved history">
      <Button
        onClick={() => {
          setBusy(true);
          setError('');
          setOpen((value) => !value);
        }}
      >
        {open ? 'Hide saved history' : 'Browse saved history'}
      </Button>
      {open && (
        <>
          <p>
            Saved exchanges, newest first. Browsing does not change the assistant’s working context.
          </p>
          {busy && <output>Loading saved history…</output>}
          {error && <p role="alert">{error}</p>}
          {!busy && page?.exchanges.length === 0 && <p>No eligible saved exchanges.</p>}
          {page?.exchanges.map((exchange) => (
            <article className="memory-source" key={exchange.operation.id}>
              <p>{exchange.operation.state}</p>
              {[exchange.input, ...(exchange.assistant ? [exchange.assistant] : [])].map(
                (record) => (
                  <div key={record.source.id}>
                    <p>
                      {record.role}
                      {record.observed_at !== null &&
                        ` · ${new Date(record.observed_at).toLocaleString()}`}
                    </p>
                    <pre>{record.source.text}</pre>
                  </div>
                ),
              )}
            </article>
          ))}
          <Button
            disabled={busy || before === null}
            onClick={() => {
              setBusy(true);
              setError('');
              setBefore(null);
            }}
          >
            Newest exchanges
          </Button>
          <Button
            disabled={busy || !page?.next_before}
            onClick={() => {
              setBusy(true);
              setError('');
              setBefore(page?.next_before ?? null);
            }}
          >
            Older exchanges
          </Button>
        </>
      )}
    </section>
  );
}
