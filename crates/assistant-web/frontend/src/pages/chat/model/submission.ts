import { useCallback, useEffect, useRef, useState } from 'react';
import { RequestError, apiVersion } from '@/shared/api';
import { findRequest, getChat, submit, lookup, terminal, useChat } from '@/entities/chat';
function identifier() {
  const bytes = crypto.getRandomValues(new Uint8Array(16));
  return Array.from(bytes, (byte) => byte.toString(16).padStart(2, '0')).join('');
}
interface Pending {
  chat: string;
  key: string;
  text: string | null;
  created: number;
}
/** HTTP acknowledgments and SSE may arrive in either order. Retries reuse a key
 * only for the same text, conversation and conservative retention window. */
export function useSubmission(onAccepted: (text: string) => void) {
  const [client] = useState(() => {
    if (apiVersion() !== 2) return identifier();
    try {
      const saved = sessionStorage.getItem('assistant.client');
      if (saved && /^[0-9a-f]{32}$/.test(saved)) return saved;
      const value = identifier();
      sessionStorage.setItem('assistant.client', value);
      return value;
    } catch {
      return identifier();
    }
  });
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState('');
  const [initialPending] = useState<Pending | null>(() => {
    if (apiVersion() !== 2) return null;
    try {
      const raw = sessionStorage.getItem('assistant.pending');
      const value: unknown = raw && raw.length < 1024 ? JSON.parse(raw) : null;
      if (
        value &&
        typeof value === 'object' &&
        'client' in value &&
        value.client === client &&
        'key' in value &&
        typeof value.key === 'string' &&
        /^[0-9a-f]{32}$/.test(value.key)
      )
        return { chat: '', key: value.key, text: null, created: 0 };
    } catch {
      /* Submission still requires a successful identity write. */
    }
    return null;
  });
  const pending = useRef<Pending | null>(initialPending);
  const reconciling = useRef(false);
  const [canDiscard, setCanDiscard] = useState(false);
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
  const clearPending = useCallback(
    (key: string) => {
      try {
        const raw = sessionStorage.getItem('assistant.pending');
        const value: unknown = raw && raw.length < 1024 ? JSON.parse(raw) : null;
        if (
          value &&
          typeof value === 'object' &&
          'key' in value &&
          value.key === key &&
          'client' in value &&
          value.client === client
        )
          sessionStorage.removeItem('assistant.pending');
        setError('');
        return true;
      } catch {
        setError(
          'The request is saved, but its recovery identity could not be cleared. It will be checked again after reload.',
        );
        return false;
      }
    },
    [client],
  );
  const accept = useCallback(
    (item: Pending) => {
      if (acknowledged.current.has(item.key)) return;
      acknowledged.current.add(item.key);
      if (acknowledged.current.size > 128)
        acknowledged.current.delete(acknowledged.current.values().next().value ?? '');
      if (pending.current?.key === item.key) pending.current = null;
      if (apiVersion() === 2) clearPending(item.key);
      else setError('');
      if (item.text !== null) callback.current(item.text);
    },
    [clearPending],
  );
  // A new run event changes the active identity; completion/snapshot also does.
  useEffect(() => {
    const item = pending.current;
    if (item && findRequest(client, item.key)) {
      accept(item);
      return;
    } else if (
      apiVersion() === 1 &&
      item &&
      chat.connection === 'online' &&
      item.chat !== chat.chatId
    ) {
      pending.current = null;
      setError('Assistant restarted. The previous submission cannot be recovered.');
    }
    if (apiVersion() === 2 && item && chat.connection === 'online' && !reconciling.current) {
      reconciling.current = true;
      void lookup(client, item.key)
        .then(() => {
          if (!mounted.current) return;
          accept(item);
        })
        .catch((failure: unknown) => {
          if (!mounted.current) return;
          if (
            failure instanceof RequestError &&
            [404, 410].includes(failure.status) &&
            item.text === null
          ) {
            setCanDiscard(true);
            setError(
              'The original request is absent or was forgotten. Discard its pending key before sending a new message.',
            );
          } else if (item.text === null)
            setError('Recovering the original request. Reconnect to check again.');
        })
        .finally(() => {
          reconciling.current = false;
        });
    }
  }, [chat, client, accept]);
  async function send(text: string) {
    if (busy || !text.trim() || getChat().connection !== 'online') return;
    const existing = pending.current;
    if (
      existing &&
      (existing.text !== text || (apiVersion() === 1 && existing.chat !== getChat().chatId))
    ) {
      setError('Resolve or retry the pending submission before sending different text.');
      return;
    }
    if (apiVersion() === 1 && existing && performance.now() - existing.created >= 540000) {
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
    if (apiVersion() === 2) {
      try {
        sessionStorage.setItem('assistant.client', client);
        sessionStorage.setItem(
          'assistant.pending',
          JSON.stringify({ namespace: 'web', client, key: item.key }),
        );
      } catch {
        setError(
          'Cannot save the request identity in this tab. Enable session storage before sending.',
        );
        return;
      }
    }
    pending.current = item;
    setBusy(true);
    setError('');
    try {
      const run = await submit(item.chat, client, item.key, text);
      if (mounted.current) {
        accept(item);
        // The event stream owns running state. A late HTTP reply cannot resurrect it.
        if (terminal(run.status) && run.error) setError(run.error);
      }
    } catch (failure: unknown) {
      if (mounted.current) {
        if (findRequest(client, item.key)) accept(item);
        else if (
          failure instanceof RequestError &&
          [400, 403, 404, 409, 413, 415, 422, 429].includes(failure.status)
        ) {
          pending.current = null;
          if (apiVersion() === 2) clearPending(item.key);
          setError(`Submission rejected (${failure.code}). Your draft is preserved.`);
        } else setError('Acknowledgment lost. Retry sends the same request safely.');
      }
    } finally {
      if (mounted.current) setBusy(false);
    }
  }
  function discardPending() {
    if (!canDiscard) return;
    if (pending.current && !clearPending(pending.current.key)) return;
    pending.current = null;
    setCanDiscard(false);
    setError('');
  }
  return { send, busy, error, canDiscard, discardPending };
}
