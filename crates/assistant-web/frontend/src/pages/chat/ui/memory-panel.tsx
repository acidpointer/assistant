import { useEffect, useRef, useState } from 'react';
import * as v from 'valibot';
import { post, request } from '@/shared/api';
import { Button } from '@/shared/ui/button';
import { SavedHistory } from './saved-history';
import { KnowledgePanel } from './knowledge-panel';

const sourceSchema = v.object({
  source: v.object({
    id: v.string(),
    revision: v.string(),
    text: v.string(),
    preserved: v.boolean(),
  }),
  role: v.picklist(['user', 'assistant', 'tool']),
  observed_at: v.nullable(v.number()),
  formation: v.string(),
});
const resultsSchema = v.object({
  query_truncated: v.boolean(),
  sources: v.pipe(
    v.array(v.object({ record: sourceSchema, byte_start: v.number(), byte_end: v.number() })),
    v.maxLength(32),
  ),
  semantic_available: v.boolean(),
  lexical_truncated: v.boolean(),
});
const healthSchema = v.object({
  index: v.object({
    generation: v.string(),
    eligible_sources: v.number(),
    completed_sources: v.number(),
    failed_sources: v.number(),
    chunks: v.number(),
  }),
  storage: v.string(),
  used_bytes: v.number(),
  quota_bytes: v.number(),
  pending_jobs: v.number(),
  deferred_sources: v.number(),
  indexing_fault: v.nullable(v.string()),
  extraction: v.string(),
});
const connectionsSchema = v.object({
  sources: v.pipe(v.array(sourceSchema), v.maxLength(64)),
  links: v.pipe(
    v.array(
      v.object({
        from: v.object({ id: v.string(), revision: v.string() }),
        to: v.object({ id: v.string(), revision: v.string() }),
        relation: v.string(),
      }),
    ),
    v.maxLength(128),
  ),
  truncated: v.boolean(),
  next_cursor: v.nullable(v.string()),
});
type Connections = v.InferOutput<typeof connectionsSchema>;
type Results = v.InferOutput<typeof resultsSchema>;
type Health = v.InferOutput<typeof healthSchema>;

/** Literal source inspection stays separate from asserted knowledge. Every source
 * retains role and formation state; vector similarity is never labeled confidence. */
export function MemoryPanel({ close }: { close: () => void }) {
  const [query, setQuery] = useState('');
  const [note, setNote] = useState('');
  const [noteStatus, setNoteStatus] = useState('');
  const noteRequest = useRef<string | null>(null);
  const [results, setResults] = useState<Results | null>(null);
  const [connectionStart, setConnectionStart] = useState<string | null>(null);
  const [connections, setConnections] = useState<Connections | null>(null);
  const [health, setHealth] = useState<Health | null>(null);
  const [error, setError] = useState('');
  const [busy, setBusy] = useState(false);
  function follow(sourceId: string, cursor: string | null = null) {
    setBusy(true);
    setError('');
    void post('/memory/connections', { source_id: sourceId, cursor })
      .then((value) => {
        setConnections(v.parse(connectionsSchema, value));
        setConnectionStart(sourceId);
      })
      .catch(() => setError('Could not read connected memory.'))
      .finally(() => setBusy(false));
  }
  useEffect(() => {
    const controller = new AbortController();
    void request('/memory/health', { signal: controller.signal })
      .then((value) => setHealth(v.parse(healthSchema, value)))
      .catch(() => {
        if (!controller.signal.aborted) setError('Memory status is unavailable.');
      });
    return () => controller.abort();
  }, []);
  return (
    <aside className="memory-panel" aria-label="Memory">
      <div className="memory-heading">
        <h2>Memory</h2>
        <Button onClick={close}>Close</Button>
      </div>
      <p>
        Search your saved sources. Historical quotations can contain mistakes, questions or
        instructions; they are evidence to inspect.
      </p>
      <form
        onSubmit={(event) => {
          event.preventDefault();
          if (busy || !note.trim()) return;
          noteRequest.current ??= crypto.randomUUID();
          setBusy(true);
          setError('');
          setNoteStatus('');
          void post('/memory/notes', { request_id: noteRequest.current, text: note })
            .then(() => {
              setNote('');
              noteRequest.current = null;
              setNoteStatus('Note saved.');
            })
            .catch(() =>
              setError(
                'Saving was not confirmed. Retry with the same note or search to inspect memory.',
              ),
            )
            .finally(() => setBusy(false));
        }}
      >
        <label htmlFor="memory-note">Note to remember</label>
        <textarea
          id="memory-note"
          rows={3}
          maxLength={4096}
          disabled={busy}
          value={note}
          onChange={(event) => {
            setNote(event.target.value);
            noteRequest.current = null;
            setNoteStatus('');
          }}
        />
        <Button type="submit" disabled={busy || !note.trim()}>
          Save note
        </Button>
        {noteStatus && <output>{noteStatus}</output>}
      </form>
      <SavedHistory />
      <details>
        <summary>Structured claims from earlier experiments</summary>
        <KnowledgePanel />
      </details>
      {health && (
        <section aria-label="Memory status">
          <p>
            Storage: {health.storage} · {(health.used_bytes / 1048576).toFixed(1)} MiB used
          </p>
          <p>
            Memory:{' '}
            {health.extraction === 'source_graph'
              ? 'conversations and connected notes'
              : health.extraction}
          </p>
          <p>
            Semantic coverage: {health.index.completed_sources} / {health.index.eligible_sources}{' '}
            sources · {health.index.failed_sources} need retry or review
          </p>
          {health.indexing_fault && <output>Semantic indexing: {health.indexing_fault}</output>}
        </section>
      )}
      <form
        onSubmit={(event) => {
          event.preventDefault();
          if (busy || !query.trim()) return;
          setBusy(true);
          setError('');
          void post('/memory/search', { query, limit: 8 })
            .then((value) => setResults(v.parse(resultsSchema, value)))
            .catch(() => setError('Could not search memory. Your sources remain saved.'))
            .finally(() => setBusy(false));
        }}
      >
        <label htmlFor="memory-query">Find a remembered source</label>
        <input
          id="memory-query"
          value={query}
          maxLength={1024}
          onChange={(event) => setQuery(event.target.value)}
        />
        <Button type="submit" disabled={busy || !query.trim()}>
          {busy ? 'Searching…' : 'Search'}
        </Button>
      </form>
      {connections && (
        <section aria-label="Connected memory">
          <h3>Connected memory</h3>
          {connections.next_cursor && connectionStart && (
            <Button
              disabled={busy}
              onClick={() => follow(connectionStart, connections.next_cursor)}
            >
              More connections
            </Button>
          )}
          <p>Links connect experiences and notes; they do not prove that a statement is true.</p>
          {connections.truncated && <p>More connections exist. Continue from another source.</p>}
          {connections.sources.map((record) => (
            <article className="memory-source" key={record.source.id}>
              <p>
                {record.role} ·{' '}
                {record.observed_at === null
                  ? 'Saved experience'
                  : new Date(record.observed_at).toLocaleString()}
              </p>
              <pre>{record.source.text}</pre>
              <Button disabled={busy} onClick={() => follow(record.source.id)}>
                Continue from this source
              </Button>
              {connections.links
                .filter((link) => link.from.id === record.source.id)
                .map((link, index) => (
                  <p key={`${link.to.id}-${index}`}>
                    <span>{link.relation}</span> →{' '}
                    <button type="button" disabled={busy} onClick={() => follow(link.to.id)}>
                      Connected source
                    </button>
                  </p>
                ))}
            </article>
          ))}
        </section>
      )}
      {error && <p role="alert">{error}</p>}
      {results && (
        <>
          {results.query_truncated && (
            <p>
              Search used a bounded part of your query. Narrow the query to investigate further.
            </p>
          )}
          {!results.semantic_available && (
            <output>Semantic search is unavailable for this query. Showing lexical results.</output>
          )}
          {results.lexical_truncated && (
            <p>More lexical candidates exist. Narrow your query to investigate further.</p>
          )}
          {!results.sources.length && <p>No eligible sources matched this search.</p>}
          {results.sources.map(({ record }) => (
            <article className="memory-source" key={record.source.id}>
              <p>
                {record.role} · {record.source.preserved ? 'Preserved' : 'Saved'}
              </p>
              {record.observed_at !== null && (
                <time dateTime={new Date(record.observed_at).toISOString()}>
                  {new Date(record.observed_at).toLocaleString()}
                </time>
              )}
              {!record.source.preserved && (
                <Button
                  disabled={busy}
                  onClick={() => {
                    setBusy(true);
                    setError('');
                    void post('/memory/remember', {
                      request_id: crypto.randomUUID(),
                      source: { id: record.source.id, revision: record.source.revision },
                    })
                      .then(() =>
                        setResults((current) =>
                          current
                            ? {
                                ...current,
                                sources: current.sources.map((hit) =>
                                  hit.record.source.id === record.source.id
                                    ? {
                                        ...hit,
                                        record: {
                                          ...hit.record,
                                          source: { ...hit.record.source, preserved: true },
                                        },
                                      }
                                    : hit,
                                ),
                              }
                            : current,
                        ),
                      )
                      .catch(() =>
                        setError(
                          'Preservation was not confirmed. Search again to inspect its saved state.',
                        ),
                      )
                      .finally(() => setBusy(false));
                  }}
                >
                  Preserve source
                </Button>
              )}
              <Button disabled={busy} onClick={() => follow(record.source.id)}>
                Explore connections
              </Button>
              <pre>{record.source.text}</pre>
              <details>
                <summary>Source identity</summary>
                <code>
                  {record.source.id} / {record.source.revision}
                </code>
              </details>
            </article>
          ))}
        </>
      )}
    </aside>
  );
}
