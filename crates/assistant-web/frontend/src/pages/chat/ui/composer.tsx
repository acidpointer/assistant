import { useCallback, useEffect, useRef, useState } from 'react';
import type { KeyboardEvent } from 'react';
import { useChat } from '@/entities/chat';
import { useSubmission } from '../model/submission';
import { StopButton } from './stop-button';
import { PromptHistory } from '../model/history';
import { Button } from '@/shared/ui/button';

/** Native textarea behavior wins during selection, multiline navigation and IME.
 * Recall copies an accepted prompt into a new draft; it never edits old messages. */
export function Composer() {
  const chat = useChat();
  const [draft, setDraft] = useState('');
  const textarea = useRef<HTMLTextAreaElement>(null);
  const history = useRef(new PromptHistory());
  const accepted = useCallback((text: string) => {
    history.current.accept(text);
    setDraft((current) => (current === text ? '' : current));
  }, []);
  const { send, busy, error, canDiscard, discardPending } = useSubmission(accepted);
  const previousInstance = useRef(chat.instanceId);
  useEffect(() => {
    if (previousInstance.current !== chat.instanceId) {
      history.current = new PromptHistory();
      previousInstance.current = chat.instanceId;
    }
  }, [chat.instanceId]);
  useEffect(() => {
    const node = textarea.current;
    if (node) {
      node.style.height = 'auto';
      node.style.height = `${Math.min(node.scrollHeight, 192)}px`;
    }
  }, [draft]);
  function keydown(event: KeyboardEvent<HTMLTextAreaElement>) {
    if (event.nativeEvent.isComposing || event.keyCode === 229) return;
    const node = event.currentTarget;
    if (event.key === 'Enter' && (!event.shiftKey || event.ctrlKey || event.metaKey)) {
      event.preventDefault();
      if (!chat.activeRunId) void send(draft);
      return;
    }
    if (
      !event.altKey &&
      !event.ctrlKey &&
      !event.metaKey &&
      !event.shiftKey &&
      node.selectionStart === node.selectionEnd
    ) {
      const recalled =
        event.key === 'ArrowUp'
          ? history.current.previous(draft)
          : event.key === 'ArrowDown'
            ? history.current.next()
            : null;
      if (recalled !== null) {
        event.preventDefault();
        setDraft(recalled);
      }
    }
    if (event.key === 'Escape' && history.current.browsing) {
      event.preventDefault();
      event.stopPropagation();
      setDraft(history.current.restore());
    }
  }
  const bytes = new TextEncoder().encode(draft).length;
  const disabled = busy || chat.connection !== 'online' || !draft.trim() || bytes > 16384;
  return (
    <form
      className="composer"
      onSubmit={(event) => {
        event.preventDefault();
        if (!disabled && !chat.activeRunId) void send(draft);
      }}
    >
      <label className="sr-only" htmlFor="prompt">
        Message Assistant
      </label>
      <textarea
        id="prompt"
        ref={textarea}
        value={draft}
        rows={1}
        placeholder="Ask anything. Build something."
        aria-describedby="composer-help"
        onKeyDown={keydown}
        onChange={(event) => {
          history.current.reset();
          setDraft(event.target.value);
        }}
      />
      <div className="composer-bottom">
        <span id="composer-help">
          Enter to send <span className="desktop-hint">· Shift + Enter for newline · ↑ recall</span>
        </span>
        {chat.activeRunId ? (
          <StopButton runId={chat.activeRunId} />
        ) : (
          <Button type="submit" className="send" aria-label="Send message" disabled={disabled}>
            {busy ? '…' : '↑'}
          </Button>
        )}
      </div>
      {(error || bytes > 16384) && (
        <p className="composer-error" role="alert">
          {bytes > 16384 ? 'Input exceeds 16 KiB.' : error}
        </p>
      )}
      {canDiscard && <Button onClick={discardPending}>Discard pending key</Button>}
    </form>
  );
}
