/** A real HTTP/SSE fixture serves the built assets under the production CSP.
 * It controls acknowledgment ordering without mocking EventSource or rendering. */
import { createServer } from 'node:http';
import type { ServerResponse } from 'node:http';
import { readFile } from 'node:fs/promises';
import { fileURLToPath } from 'node:url';
import * as v from 'valibot';
import type { Run, Message } from '../../src/entities/chat';
import { chatSnapshotSchema } from '../../src/entities/chat';
import type { ResourceSample, ResourceWindow } from '../../src/entities/telemetry';
const inputSchema = v.object({
  client_id: v.string(),
  client_request_id: v.string(),
  input: v.object({ text: v.string() }),
});
const emptyWindow: ResourceWindow = {
  started: null,
  ended: null,
  before: null,
  after: null,
  max_observed_rss_bytes: null,
  max_observed_vram_bytes: null,
};
export const sample: ResourceSample = {
  sampled_at_unix_ms: 1000,
  monotonic_ns: 1e9,
  process_cpu_ns: 1e9,
  cpu_percent: 125,
  ram_rss_bytes: 1024 ** 3,
  ram_pss_bytes: 1024 ** 3 / 2,
  vram_bytes: 2 * 1024 ** 3,
  vram_source: 'nvml_process_v2',
  ram_source: 'linux_smaps_rollup',
};
export const message = (
  id: string,
  role: Message['role'],
  text: string,
  status: Message['status'] = 'completed',
): Message => ({ id, run_id: id.split('-assistant-')[0] ?? 'fixture', role, text, status });
export async function fixture() {
  const clients = new Set<ServerResponse>();
  const submissions: v.InferOutput<typeof inputSchema>[] = [];
  const snapshot: v.InferOutput<typeof chatSnapshotSchema> & {
    resources: ResourceSample;
    inference: { generated_tokens: number; generation_ns: number };
  } = {
    instance_id: 'instance',
    chat_id: 'primary',
    revision: 'instance:0',
    messages: [],
    active_run: null,
    recent_runs: [],
    truncated: false,
    resources: sample,
    inference: { generated_tokens: 0, generation_ns: 0 },
  };
  let sequence = 0;
  function send(type: string, data: unknown) {
    sequence += 1;
    for (const client of clients)
      client.write(`id: instance:${sequence}\nevent: ${type}\ndata: ${JSON.stringify(data)}\n\n`);
  }
  function complete(text = 'Already complete', status: Run['status'] = 'completed') {
    const run = snapshot.active_run;
    if (!run) throw new Error('No fixture run');
    const answer = snapshot.messages.find((item) => item.id === `${run.id}-assistant-1`);
    if (answer) {
      answer.text = text;
      answer.status = status;
      send('update', { type: 'message', message: answer });
    }
    run.status = status;
    run.generated_tokens = 50;
    run.performance = [
      {
        measurement_complete: true,
        turn: 1,
        generated_tokens: 50,
        timing: { prefill_ns: 1e6, generation_ns: 1e9, elapsed_ns: 1.1e9, first_token_ns: 1e8 },
        resources: {
          ...emptyWindow,
          started: { monotonic_ns: 1e9, process_cpu_ns: 1e9 },
          ended: { monotonic_ns: 2e9, process_cpu_ns: 2.25e9 },
          before: sample,
          after: { ...sample, monotonic_ns: 2e9, process_cpu_ns: 2.25e9 },
          max_observed_rss_bytes: sample.ram_rss_bytes,
          max_observed_vram_bytes: sample.vram_bytes,
        },
      },
    ];
    snapshot.active_run = null;
    snapshot.inference.generated_tokens += 50;
    snapshot.inference.generation_ns += 1e9;
    send('update', { type: 'run', run });
    send('update', { type: 'telemetry', sample, inference: snapshot.inference });
  }
  const server = createServer((req, res) => {
    void (async () => {
      const url = new URL(req.url ?? '/', 'http://localhost');
      if (url.pathname === '/' || /^\/assets\/[a-zA-Z0-9_.-]+$/.test(url.pathname)) {
        const name = url.pathname === '/' ? 'index.html' : url.pathname.slice(1);
        const bytes = await readFile(fileURLToPath(new URL(`../../dist/${name}`, import.meta.url)));
        res.setHeader(
          'Content-Type',
          name.endsWith('.html')
            ? 'text/html'
            : name.endsWith('.js')
              ? 'text/javascript'
              : name.endsWith('.css')
                ? 'text/css'
                : 'font/woff2',
        );
        res.setHeader(
          'Content-Security-Policy',
          "default-src 'none'; script-src 'self'; style-src 'self'; font-src 'self'; worker-src 'self'; connect-src 'self'; base-uri 'none'; form-action 'self'; frame-ancestors 'none'",
        );
        res.end(bytes);
        return;
      }
      if (url.pathname.endsWith('/events')) {
        res.writeHead(200, { 'Content-Type': 'text/event-stream', 'Cache-Control': 'no-store' });
        res.write(`event: snapshot\ndata: ${JSON.stringify(snapshot)}\n\n`);
        clients.add(res);
        req.on('close', () => clients.delete(res));
        return;
      }
      res.setHeader('Content-Type', 'application/json');
      if (url.pathname === '/api/v1/bootstrap') {
        res.end(
          JSON.stringify({ primary_chat_id: 'primary', limits: { deduplication_seconds: 600 } }),
        );
        return;
      }
      if (url.pathname.endsWith('/cancel')) {
        const run = snapshot.active_run;
        complete(snapshot.messages[snapshot.messages.length - 1]?.text ?? '', 'cancelled');
        res.end(JSON.stringify(run));
        return;
      }
      if (url.pathname.endsWith('/runs') && req.method === 'POST') {
        const chunks = [];
        for await (const chunk of req) chunks.push(Buffer.from(chunk));
        const body: unknown = JSON.parse(Buffer.concat(chunks).toString());
        const input = v.parse(inputSchema, body);
        submissions.push(input);
        if (input.input.text === 'lost-both') {
          res.destroy();
          return;
        }
        const id = `run${submissions.length}`;
        const run: Run = {
          id,
          chat_id: 'primary',
          client_id: input.client_id,
          client_request_id: input.client_request_id,
          status: 'running',
          error: null,
          input_tokens: 20,
          generated_tokens: 0,
          warning_count: 0,
          unknown_tool_outcome: false,
          limit_reason: null,
          performance: [],
          resources: emptyWindow,
        };
        snapshot.active_run = run;
        snapshot.recent_runs.push(run);
        const entries: Message[] = [
          { id: `${id}-user`, run_id: id, role: 'user', text: input.input.text, status: 'running' },
          { id: `${id}-assistant-1`, run_id: id, role: 'assistant', text: '', status: 'running' },
        ];
        snapshot.messages.push(...entries);
        send('update', { type: 'run', run });
        entries.forEach((item) => send('update', { type: 'message', message: item }));
        const acknowledgment = { ...run };
        if (input.input.text === 'finish-before-ack') complete();
        if (['drop-ack', 'finish-before-ack'].includes(input.input.text))
          await new Promise((resolve) => setTimeout(resolve, 100));
        if (input.input.text === 'drop-ack') {
          res.destroy();
          return;
        }
        res.writeHead(202);
        res.end(JSON.stringify(acknowledgment));
        return;
      }
      res.writeHead(404);
      res.end('{}');
    })().catch(() => {
      res.writeHead(500);
      res.end();
    });
  });
  await new Promise<void>((resolve) => server.listen(0, '127.0.0.1', resolve));
  const address = server.address();
  if (!address || typeof address === 'string') throw new Error('No fixture address');
  return {
    snapshot,
    submissions,
    send,
    complete,
    base: `http://127.0.0.1:${address.port}`,
    reconnect() {
      for (const client of clients) client.end();
    },
    async close() {
      for (const client of clients) client.end();
      server.closeAllConnections();
      await new Promise<void>((resolve) => server.close(() => resolve()));
    },
  };
}
