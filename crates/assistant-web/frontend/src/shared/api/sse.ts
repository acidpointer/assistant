/** Incremental bounded SSE decoder. Network chunks may split UTF-8, CRLF and
 * fields anywhere. EOF discards unfinished events, matching the HTML standard. */
export async function readEvents(
  body: ReadableStream<Uint8Array>,
  signal: AbortSignal,
  receive: (event: string, data: string) => void,
) {
  const reader = body.getReader();
  const decoder = new TextDecoder('utf-8', { fatal: true });
  let buffer = '';
  let data: string[] = [];
  let type = '';
  let size = 0;
  let skipLf = false;
  const abort = () => {
    void reader.cancel().catch(() => {});
  };
  signal.addEventListener('abort', abort, { once: true });
  function line(value: string) {
    size += value.length;
    if (size > 8 * 1024 * 1024) throw new Error('Event exceeds byte allowance');
    if (!value) {
      if (data.length) receive(type || 'message', data.join('\n'));
      data = [];
      type = '';
      size = 0;
      return;
    }
    if (value.startsWith(':')) return;
    const colon = value.indexOf(':');
    const field = colon < 0 ? value : value.slice(0, colon);
    let content = colon < 0 ? '' : value.slice(colon + 1);
    if (content.startsWith(' ')) content = content.slice(1);
    if (field === 'event') type = content;
    else if (field === 'data') data.push(content);
  }
  try {
    while (!signal.aborted) {
      const item = await reader.read();
      if (item.done) break;
      // The byte bound is conservative even if input is a single huge chunk.
      if (item.value.byteLength + buffer.length * 3 + size * 3 > 24 * 1024 * 1024)
        throw new Error('Event exceeds byte allowance');
      buffer += decoder.decode(item.value, { stream: true });
      let start = 0;
      for (let i = 0; i < buffer.length; i += 1) {
        const char = buffer[i];
        if (skipLf) {
          skipLf = false;
          if (char === '\n') {
            start = i + 1;
            continue;
          }
        }
        if (char === '\n' || char === '\r') {
          line(buffer.slice(start, i));
          start = i + 1;
          skipLf = char === '\r';
        }
      }
      buffer = buffer.slice(start);
      if (buffer.length + size > 8 * 1024 * 1024) throw new Error('Event exceeds byte allowance');
    }
  } finally {
    signal.removeEventListener('abort', abort);
    await reader.cancel().catch(() => {});
    reader.releaseLock();
  }
}
