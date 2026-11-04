import { useChat, useRun } from '@/entities/chat';
import { memory, tokensPerSecond, useTelemetry } from '@/entities/telemetry';
import type { Performance, ResourceWindow } from '@/entities/telemetry';
function rate(tokens: number, ns: number) {
  const result = tokensPerSecond(tokens, ns);
  return result === null ? '—' : result.toFixed(1);
}
function cpu(window: ResourceWindow): string {
  const before = window.started,
    after = window.ended;
  if (!before || !after || before.process_cpu_ns === null || after.process_cpu_ns === null)
    return 'Unavailable';
  const wall = after.monotonic_ns - before.monotonic_ns;
  return wall > 0
    ? `${(((after.process_cpu_ns - before.process_cpu_ns) / wall) * 100).toFixed(1)}%`
    : '—';
}
export function OverallPerformance() {
  const { sample, totals } = useTelemetry();
  const chat = useChat();
  const active = useRun(chat.activeRunId);
  const measured = active?.performance.filter((turn) => turn.timing !== null) ?? [];
  const liveTokens = measured.reduce((sum, turn) => sum + turn.generated_tokens, 0);
  const liveTime = measured.reduce((sum, turn) => sum + (turn.timing?.generation_ns ?? 0), 0);
  return (
    <details className="overall-metrics">
      <summary aria-label="Performance details">
        <span>
          <b>
            {rate(
              active ? liveTokens : totals.generated_tokens,
              active ? liveTime : totals.generation_ns,
            )}
          </b>{' '}
          tok/s
        </span>
        <span className="metric-divider" />
        <span>
          VRAM <b>{memory(sample?.vram_bytes)}</b>
        </span>
      </summary>
      <div className="metrics-popover">
        <strong>Daemon resources</strong>
        <p>
          RAM (RSS) <b>{memory(sample?.ram_rss_bytes)}</b>
        </p>
        <p>
          RAM (PSS) <b>{memory(sample?.ram_pss_bytes)}</b>
        </p>
        <p>
          CPU <b>{sample?.cpu_percent?.toFixed(1) ?? '—'}%</b>
        </p>
        <small>
          100% CPU = one core. Throughput uses active turns while running; otherwise the weighted
          lifetime rate.
        </small>
        <small>
          VRAM: {sample?.vram_source ?? 'provider unavailable'}. RAM:{' '}
          {sample?.ram_source ?? 'unavailable'}.
        </small>
        <small>
          {sample?.sampled_at_unix_ms != null
            ? `Observed ${new Date(sample.sampled_at_unix_ms).toLocaleTimeString()}`
            : 'Waiting for measurement'}
        </small>
      </div>
    </details>
  );
}
export function MessagePerformance({ performance }: { performance: Performance | undefined }) {
  if (!performance) return null;
  const timing = performance.timing;
  return (
    <details className="message-metrics">
      <summary>
        {rate(performance.generated_tokens, timing?.generation_ns ?? 0)} tok/s
        <span> · VRAM {memory(performance.resources.max_observed_vram_bytes)}</span>
      </summary>
      <div className="message-metric-details">
        {timing && !performance.measurement_complete && (
          <small>
            Partial measurement through the last native report; interrupted work may include an
            unreported final step.
          </small>
        )}
        <span>{performance.generated_tokens} sampled tokens (includes reasoning/protocol)</span>
        <span>RAM max observed {memory(performance.resources.max_observed_rss_bytes)}</span>
        <span>CPU during message window {cpu(performance.resources)}</span>
        <span>
          Prefill {timing ? (timing.prefill_ns / 1e6).toFixed(1) : '—'} ms · Generation{' '}
          {timing ? (timing.generation_ns / 1e6).toFixed(1) : '—'} ms
        </span>
        <span>
          First sampled token{' '}
          {timing?.first_token_ns == null ? '—' : `${(timing.first_token_ns / 1e6).toFixed(1)} ms`}
        </span>
        <small>
          Memory is the daemon's observed footprint during this message, including resident weights.
          It is not an exclusive allocation cost or a continuous peak.
        </small>
      </div>
    </details>
  );
}
