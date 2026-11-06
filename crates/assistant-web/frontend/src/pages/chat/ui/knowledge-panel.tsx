import { useEffect, useState } from 'react';
import * as v from 'valibot';
import { post } from '@/shared/api';
import { Button } from '@/shared/ui/button';

const object = v.variant('kind', [
  v.object({ kind: v.literal('reference'), value: v.string() }),
  v.object({ kind: v.literal('text'), value: v.string() }),
  v.object({ kind: v.literal('timestamp'), value: v.number() }),
]);
const graphSchema = v.object({
  sequence: v.nullable(v.string()),
  truncated: v.boolean(),
  entities: v.pipe(
    v.array(v.object({ id: v.string(), kind: v.string(), label: v.string() })),
    v.maxLength(513),
  ),
  claims: v.pipe(
    v.array(
      v.object({
        id: v.string(),
        revision: v.string(),
        input: v.object({
          subject: v.string(),
          predicate: v.string(),
          object,
          negative: v.boolean(),
          modality: v.string(),
          valid_from: v.nullable(v.number()),
          valid_to: v.nullable(v.number()),
          conditions: v.pipe(v.array(v.string()), v.maxLength(4)),
          evidence: v.pipe(
            v.array(
              v.object({ source_id: v.string(), source_revision: v.string(), quote: v.string() }),
            ),
            v.maxLength(4),
          ),
        }),
      }),
    ),
    v.maxLength(256),
  ),
  sources: v.pipe(
    v.array(
      v.object({
        source: v.object({ id: v.string(), revision: v.string() }),
        role: v.picklist(['user', 'assistant', 'tool']),
        observed_at: v.nullable(v.number()),
      }),
    ),
    v.maxLength(1024),
  ),
  paths: v.pipe(
    v.array(
      v.object({
        claim: v.string(),
        steps: v.pipe(
          v.array(
            v.object({
              claim: v.string(),
              revision: v.string(),
              from: v.string(),
              to: v.nullable(v.string()),
            }),
          ),
          v.maxLength(12),
        ),
      }),
    ),
    v.maxLength(256),
  ),
});
type Graph = v.InferOutput<typeof graphSchema>;

function parseGraph(value: unknown): Graph {
  const graph = v.parse(graphSchema, value);
  const nodes = new Set(graph.entities.map((entity) => entity.id));
  const sources = new Set(
    graph.sources.map((source) => `${source.source.id}:${source.source.revision}`),
  );
  const claims = new Set(graph.claims.map((claim) => `${claim.id}:${claim.revision}`));
  for (const claim of graph.claims) {
    if (
      !nodes.has(claim.input.subject) ||
      (claim.input.object.kind === 'reference' && !nodes.has(claim.input.object.value)) ||
      claim.input.evidence.some(
        (evidence) => !sources.has(`${evidence.source_id}:${evidence.source_revision}`),
      )
    )
      throw new Error('Incomplete graph attribution');
  }
  for (const path of graph.paths)
    if (path.steps.some((step) => !claims.has(`${step.claim}:${step.revision}`)))
      throw new Error('Incomplete navigation provenance');
  return graph;
}

function time(value: number | null) {
  if (value === null) return 'unknown';
  const date = new Date(value);
  return Number.isFinite(date.getTime()) ? date.toLocaleString() : `timestamp ${value}`;
}

/** Original assertions and quotation roles stay visible beside navigation.
 * Following an incoming edge never changes its meaning or implies causality. */
export function KnowledgePanel() {
  const [open, setOpen] = useState(false);
  const [start, setStart] = useState<string | null>(null);
  const [graph, setGraph] = useState<Graph | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState('');
  useEffect(() => {
    if (!open) return;
    let current = true;
    void post('/memory/follow', { start, depth: 2 })
      .then((value) => {
        if (current) setGraph(parseGraph(value));
      })
      .catch(() => {
        if (current)
          setError('Relationships are unavailable. Saved sources can still be searched.');
      })
      .finally(() => {
        if (current) setBusy(false);
      });
    return () => {
      current = false;
    };
  }, [open, start]);
  function follow(id: string | null) {
    setBusy(true);
    setError('');
    setStart(id);
  }
  function label(id: string) {
    return graph?.entities.find((entity) => entity.id === id)?.label ?? id;
  }
  return (
    <section aria-label="Relationships">
      <Button
        onClick={() => {
          setBusy(true);
          setError('');
          setOpen((value) => !value);
        }}
      >
        {open ? 'Hide relationships' : 'Inspect relationships'}
      </Button>
      {open && (
        <>
          <p>
            Assertions retain their original direction, qualifications and sources. Navigation paths
            do not prove additional facts.
          </p>
          {busy && <output>Loading relationships…</output>}
          {error && <p role="alert">{error}</p>}
          {!busy && graph && (
            <>
              <Button disabled={start === null} onClick={() => follow(null)}>
                Start from me
              </Button>
              {!graph.claims.length && (
                <p>
                  No eligible structured assertions at this node. Saved quotations remain available
                  in source search.
                </p>
              )}
              {graph.truncated && (
                <p>
                  The navigation limit was reached. Follow a specific reference to investigate
                  further.
                </p>
              )}
              {graph.claims.map((claim) => (
                <article className="memory-source" key={claim.id}>
                  <p>
                    <strong>{label(claim.input.subject)}</strong> ·{' '}
                    {claim.input.negative ? 'does not: ' : ''}
                    {claim.input.predicate.replace(/_/g, ' ')} ·{' '}
                    {claim.input.object.kind === 'reference'
                      ? label(claim.input.object.value)
                      : claim.input.object.kind === 'timestamp'
                        ? time(claim.input.object.value)
                        : claim.input.object.value}
                  </p>
                  <p>
                    {claim.input.modality} · valid from {time(claim.input.valid_from)} · until{' '}
                    {time(claim.input.valid_to)}
                  </p>
                  {claim.input.conditions.map((condition, index) => (
                    <p key={index}>Condition: {condition}</p>
                  ))}
                  {claim.input.evidence.map((evidence) => {
                    const attribution = graph.sources.find(
                      (source) =>
                        source.source.id === evidence.source_id &&
                        source.source.revision === evidence.source_revision,
                    );
                    return (
                      <blockquote key={`${evidence.source_id}:${evidence.source_revision}`}>
                        <p>
                          {attribution?.role} · observed {time(attribution?.observed_at ?? null)}
                        </p>
                        <pre>{evidence.quote}</pre>
                      </blockquote>
                    );
                  })}
                  <details>
                    <summary>Navigation provenance</summary>
                    <p>
                      Assertion {claim.id} · revision {claim.revision}
                    </p>
                    <ol>
                      {graph.paths
                        .find((path) => path.claim === claim.id)
                        ?.steps.map((step, index) => (
                          <li key={index}>
                            {label(step.from)} → {step.to ? label(step.to) : 'literal value'} ·
                            assertion {step.claim} / {step.revision}
                          </li>
                        ))}
                    </ol>
                  </details>
                  {claim.input.object.kind === 'reference' && (
                    <Button
                      disabled={start === claim.input.object.value}
                      onClick={() =>
                        follow(
                          claim.input.object.kind === 'reference' ? claim.input.object.value : null,
                        )
                      }
                    >
                      Follow reference
                    </Button>
                  )}
                  <Button
                    disabled={start === claim.input.subject}
                    onClick={() => follow(claim.input.subject)}
                  >
                    Follow subject
                  </Button>
                </article>
              ))}
            </>
          )}
        </>
      )}
    </section>
  );
}
