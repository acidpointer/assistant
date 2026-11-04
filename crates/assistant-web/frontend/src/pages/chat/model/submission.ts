import { useEffect, useRef, useState } from 'react';
import { RequestError } from '@/shared/api';
import { findRequest, getChat, submit, terminal, useChat } from '@/entities/chat';
function identifier() {
  const bytes = crypto.getRandomValues(new Uint8Array(16));
  return Array.from(bytes, (byte) => byte.toString(16).padStart(2, '0')).join('');
}
interface Pending {
  chat: string;
  key: string;
  text: string;
  created: number;
}
/** HTTP acknowledgments and SSE may arrive in either order. Retries reuse a key
 * only for the same text, conversation and conservative retention window. */
export function useSubmission(onAccepted: (text: string) => void) {
  const [client] = useState(identifier);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState('');
  const pending = useRef<Pending | null>(null);
  const acknowledged = useRef(new Set<string>());
  const mounted = useRef(true);
  const chat = useChat();
  const callback = useRef(onAccepted);
  useEffect(() => {
    callback.current = onAccepted;
  }, [onAccepted]);
  useEffect(() => {
    mounted.current = true;
    return () => {
      mounted.current = false;
    };
  }, []);
  function accept(item: Pending) {
    if (acknowledged.current.has(item.key)) return;
    acknowledged.current.add(item.key);
    if (acknowledged.current.size > 128)
      acknowledged.current.delete(acknowledged.current.values().next().value ?? '');
    if (pending.current?.key === item.key) pending.current = null;
    callback.current(item.text);
  }
  // A new run event changes the active identity; completion/snapshot also does.
  useEffect(() => {
    const item = pending.current;
    if (item && findRequest(client, item.key)) {
      accept(item);
      setError('');
    } else if (item && chat.connection === 'online' && item.chat !== chat.chatId) {
      pending.current = null;
      setError('Assistant restarted. The previous submission cannot be recovered.');
    }
  }, [chat, client]);
  async function send(text: string) {
    if (busy || !text.trim() || getChat().connection !== 'online') return;
    const existing = pending.current;
    if (existing && (existing.text !== text || existing.chat !== getChat().chatId)) {
      setError('Resolve or retry the pending submission before sending different text.');
      return;
    }
    if (existing && performance.now() - existing.created >= 540000) {
      setError(
        'Retry window expired. Reconnect and inspect the conversation before sending again.',
      );
      return;
    }
    const item = existing ?? {
      chat: getChat().chatId,
      key: identifier(),
      text,
      created: performance.now(),
    };
    pending.current = item;
    setBusy(true);
    setError('');
    try {
      const run = await submit(item.chat, client, item.key, text);
      if (mounted.current) {
        accept(item);
        // The event stream owns running state. A late HTTP reply cannot resurrect it.
        if (terminal(run.status)) setError(run.error ?? '');
      }
    } catch (failure: unknown) {
      if (mounted.current) {
        if (findRequest(client, item.key)) accept(item);
        else if (
          failure instanceof RequestError &&
          [400, 403, 404, 409, 413, 415, 422, 429].includes(failure.status)
        ) {
          pending.current = null;
          setError(`Submission rejected (${failure.code}). Your draft is preserved.`);
        } else setError('Acknowledgment lost. Retry sends the same request safely.');
      }
    } finally {
      if (mounted.current) setBusy(false);
    }
  }
  return { send, busy, error };
}
