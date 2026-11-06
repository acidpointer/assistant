import { describe, expect, it } from 'vitest';
import { readEvents } from '../src/shared/api/sse';

function bytes(chunks: Uint8Array[]) {
  return new ReadableStream<Uint8Array>({
    start(controller) {
      chunks.forEach((chunk) => controller.enqueue(chunk));
      controller.close();
    },
  });
}
describe('authenticated SSE byte boundary handling', () => {
  it('handles every byte split across UTF-8, BOM, CRLF, CR, fields and multiline data', async () => {
    const input = new TextEncoder().encode(
      '\ufeff: heartbeat\r\nevent: update\r\ndata: Україна\r\ndata: 東京\r\n\r\nevent: snapshot\rdata: {}\r\r',
    );
    const seen: [string, string][] = [];
    await readEvents(
      bytes(Array.from(input, (byte) => Uint8Array.of(byte))),
      new AbortController().signal,
      (type, data) => seen.push([type, data]),
    );
    expect(seen).toEqual([
      ['update', 'Україна\n東京'],
      ['snapshot', '{}'],
    ]);
  });
  it('discards an incomplete final event and ignores comments and unknown fields', async () => {
    const seen: string[] = [];
    await readEvents(
      bytes([
        new TextEncoder().encode(
          'retry: 1\n: hello\nunknown: ignored\ndata: complete\n\ndata: incomplete',
        ),
      ]),
      new AbortController().signal,
      (_, data) => seen.push(data),
    );
    expect(seen).toEqual(['complete']);
  });
  it('cancels a blocked reader when its session is revoked', async () => {
    let cancelled = false;
    const body = new ReadableStream<Uint8Array>({
      cancel() {
        cancelled = true;
      },
    });
    const controller = new AbortController();
    const running = readEvents(body, controller.signal, () => {
      throw new Error('no private publication');
    });
    controller.abort();
    await running;
    expect(cancelled).toBe(true);
  });
  it('rejects unbounded unfinished data before a snapshot can reach consumers', async () => {
    const body = bytes([new TextEncoder().encode(`data: ${'x'.repeat(8 * 1024 * 1024 + 1)}`)]);
    await expect(
      readEvents(body, new AbortController().signal, () => {
        throw new Error('not reached');
      }),
    ).rejects.toThrow('allowance');
  });
});
