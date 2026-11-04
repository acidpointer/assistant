import { memo } from 'react';
import { terminal, useMessage, useRun } from '@/entities/chat';
import { MessagePerformance } from './performance';
import { Markdown } from '@/shared/ui/markdown';
const reasons: Record<string, string> = {
  output_tokens: 'Response token limit reached.',
  context_capacity: 'Context capacity reached.',
  run_tokens: 'Interaction token allowance reached.',
};
export const MessageRow = memo(function MessageRow({ id }: { id: string }) {
  const message = useMessage(id);
  const run = useRun(message?.run_id ?? null);
  if (!message) return null;
  const turn = Number(id.slice(id.lastIndexOf('-') + 1));
  const performance = run?.performance.find((item) => item.turn === turn);
  const hasLaterTurn = run?.performance.some((item) => item.turn > turn) ?? false;
  const complete = terminal(message.status) || hasLaterTurn;
  return (
    <article
      className={`message message-${message.role}`}
      data-message-id={id}
      aria-label={`${message.role} message`}
    >
      <div className="message-label">
        <span className={`avatar avatar-${message.role}`}>
          {message.role === 'assistant' ? '✦' : 'Y'}
        </span>
        <span>{message.role === 'assistant' ? 'Assistant' : 'You'}</span>
        {message.role === 'assistant' && !complete && (
          <span className="live-dot" aria-label="Generating" />
        )}
      </div>
      <div className="message-body">
        {message.role === 'assistant' ? (
          <Markdown id={id} text={message.text} complete={complete} />
        ) : (
          <div className="user-text">{message.text}</div>
        )}
        {message.role === 'assistant' && <MessagePerformance performance={performance} />}
        {message.role === 'assistant' &&
          complete &&
          !hasLaterTurn &&
          message.status !== 'completed' && (
            <output className="outcome">
              {message.status === 'truncated'
                ? (reasons[run?.limit_reason ?? ''] ?? 'Generation stopped at a token boundary.')
                : `Generation ${message.status.replace('_', ' ')}.`}
              {run?.error && ` (${run.error})`}
            </output>
          )}
      </div>
    </article>
  );
});
