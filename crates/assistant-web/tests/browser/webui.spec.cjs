/** Browser integration against an in-memory HTTP/SSE fixture. It serves the
 * production HTML and CSP, exercising actual network ordering and DOM behavior. */
const { test, expect } = require('@playwright/test');
const http = require('node:http');
const fs = require('node:fs');
const path = require('node:path');

let server, base, clients, snapshot, submissions, sequence;
const message = (id, role, text, status = 'completed') => ({ id, run_id: 'run', role, text, status });
function send(type, data) {
  sequence++;
  for (const client of clients) client.write(`id: instance:${sequence}\nevent: ${type}\ndata: ${JSON.stringify(data)}\n\n`);
}
test.beforeEach(async () => {
  clients = new Set(); submissions = []; sequence = 0;
  snapshot = { instance_id: 'instance', chat_id: 'primary', revision: 'instance:0', messages: [], active_run: null, recent_runs: [], truncated: false };
  server = http.createServer(async (req, res) => {
    if (req.url === '/') {
      res.setHeader('content-type', 'text/html');
      res.setHeader('content-security-policy', "default-src 'none'; script-src 'nonce-test'; style-src 'nonce-test'; connect-src 'self'; base-uri 'none'");
      res.end(fs.readFileSync(path.join(__dirname, '../../webui.html'), 'utf8').replaceAll('__CSP_NONCE__', 'test')); return;
    }
    if (req.url.endsWith('/events')) {
      res.writeHead(200, { 'content-type': 'text/event-stream', 'cache-control': 'no-store' });
      res.write(`event: snapshot\ndata: ${JSON.stringify(snapshot)}\n\n`);
      clients.add(res); req.on('close', () => clients.delete(res)); return;
    }
    res.setHeader('content-type', 'application/json');
    if (req.url === '/api/v1/bootstrap') {
      res.end(JSON.stringify({ api_version: 1, primary_chat_id: 'primary', instance_id: snapshot.instance_id, limits: { input_bytes: 16384 } })); return;
    }
    if (req.url === '/api/v1/chats/primary' && req.method === 'GET') { res.end(JSON.stringify(snapshot)); return; }
    if (req.url.endsWith('/cancel')) {
      snapshot.active_run.status = 'cancelled';
      const result = snapshot.active_run;
      send('update', { type: 'run', run: result }); snapshot.active_run = null;
      res.end(JSON.stringify(result)); return;
    }
    if (req.url.endsWith('/runs') && req.method === 'POST') {
      let body = ''; for await (const chunk of req) body += chunk;
      const input = JSON.parse(body); submissions.push(input);
      if (input.input.text === 'lost-both') { res.destroy(); return; }
      const run = { id: 'run', chat_id: 'primary', client_id: input.client_id, client_request_id: input.client_request_id, status: 'running', error: null };
      snapshot.active_run = run; snapshot.recent_runs = [run];
      snapshot.messages = [message('user', 'user', input.input.text, 'running'), message('answer', 'assistant', '', 'running')];
      send('update', { type: 'run', run });
      for (const item of snapshot.messages) send('update', { type: 'message', message: item });
      const acknowledgment = Object.assign({}, run);
      if (input.input.text === 'finish-before-ack') {
        snapshot.messages[1].text = 'Already complete'; snapshot.messages[1].status = 'completed';
        run.status = 'completed'; snapshot.active_run = null;
        send('update', { type: 'message', message: snapshot.messages[1] });
        send('update', { type: 'run', run });
        await new Promise(resolve => setTimeout(resolve, 100));
      }
      if (input.input.text === 'drop-ack') {
        await new Promise(resolve => setTimeout(resolve, 100));
        res.destroy(); return;
      }
      res.writeHead(202); res.end(JSON.stringify(acknowledgment)); return;
    }
    res.writeHead(404); res.end('{}');
  });
  await new Promise(resolve => server.listen(0, '127.0.0.1', resolve));
  base = `http://127.0.0.1:${server.address().port}`;
});
test.afterEach(async () => {
  for (const client of clients) client.end();
  server.closeAllConnections();
  await new Promise(resolve => server.close(resolve));
});
async function ready(page) {
  await page.goto(base);
  await expect(page.locator('#connection')).toHaveText('Connected');
}

test('send, incremental plain-text rendering, cancellation, and draft preservation', async ({ page }) => {
  const errors = []; page.on('pageerror', error => errors.push(error.message));
  await ready(page);
  await page.locator('#input').fill('Hello');
  await page.locator('#submit').click();
  await expect(page.locator('#submit')).toHaveText('Stop');
  await expect(page.locator('#input')).toHaveValue('');
  expect(submissions).toHaveLength(1);
  await page.locator('#input').fill('Next question');
  snapshot.messages[1].text = '<img src=x onerror=alert(1)> literal';
  send('update', { type: 'delta', message_id: 'answer', text: snapshot.messages[1].text });
  await expect(page.locator('[data-role=assistant] .message-text')).toHaveText(snapshot.messages[1].text);
  expect(await page.locator('#messages img').count()).toBe(0);
  await page.locator('#submit').click();
  await expect(page.locator('#submit')).toHaveText('Send');
  await expect(page.locator('#input')).toHaveValue('Next question');
  expect(errors).toEqual([]);
});

test('IME composition and Shift+Enter do not submit', async ({ page }) => {
  await ready(page); await page.locator('#input').fill('Composing');
  await page.locator('#input').dispatchEvent('keydown', { key: 'Enter', isComposing: true, keyCode: 229 });
  await page.locator('#input').press('Shift+Enter');
  expect(submissions).toHaveLength(0);
  await expect(page.locator('#input')).toHaveValue('Composing\n');
});

test('a late HTTP acknowledgment cannot revive a completed response', async ({ page }) => {
  await ready(page); await page.locator('#input').fill('finish-before-ack');
  await page.locator('#submit').click();
  await expect(page.locator('[data-role=assistant] .message-text')).toHaveText('Already complete');
  await expect(page.locator('#submit')).toHaveText('Send');
  await expect(page.locator('#feedback')).not.toHaveAttribute('data-error', 'true');
});

test('stream acknowledgment resolves a lost HTTP reply without resubmission', async ({ page }) => {
  await ready(page); await page.locator('#input').fill('drop-ack');
  await page.locator('#submit').click();
  await expect(page.locator('#submit')).toHaveText('Stop');
  await expect(page.locator('#input')).toHaveValue('');
  await expect(page.locator('#feedback')).not.toHaveAttribute('data-error', 'true');
  expect(submissions).toHaveLength(1);
});

test('wall-clock changes cannot replace an ambiguous submission key', async ({ page }) => {
  await ready(page); await page.locator('#input').fill('lost-both');
  await page.locator('#submit').click();
  await expect(page.locator('#feedback')).toHaveAttribute('data-error', 'true');
  const first = submissions[0].client_request_id;
  await page.evaluate(() => { const now = Date.now(); Date.now = () => now + 3600000; });
  await page.locator('#submit').click();
  await expect.poll(() => submissions.length).toBe(2);
  expect(submissions[1].client_request_id).toBe(first);
});

test('reconnect replaces state without resubmitting and preserves the draft', async ({ page }) => {
  await ready(page); await page.locator('#input').fill('Keep this draft');
  snapshot.messages = [message('external', 'assistant', 'Arrived elsewhere')];
  for (const client of clients) client.end();
  await expect(page.locator('.message-text')).toHaveText('Arrived elsewhere', { timeout: 15000 });
  await expect(page.locator('#input')).toHaveValue('Keep this draft');
  expect(submissions).toHaveLength(0);
});

test('an inconsistent delta recovers through a snapshot without a stale error banner', async ({ page }) => {
  await ready(page);
  snapshot.messages = [message('recovered', 'assistant', 'Recovered state')];
  send('update', { type: 'delta', message_id: 'unknown', text: 'out of sequence' });
  await expect(page.locator('.message-text')).toHaveText('Recovered state');
  await expect(page.locator('#connection')).toHaveText('Connected');
  await expect(page.locator('#feedback')).not.toHaveAttribute('data-error', 'true');
  expect(submissions).toHaveLength(0);
});

for (const width of [320, 768, 2560]) {
  test(`layout stays usable at ${width}px`, async ({ page }) => {
    await page.setViewportSize({ width, height: 640 });
    snapshot.messages = [message('long', 'assistant', 'unbroken'.repeat(1000))];
    await ready(page);
    const dimensions = await page.evaluate(() => ({
      overflow: document.documentElement.scrollWidth > window.innerWidth,
      composer: document.querySelector('#input').getBoundingClientRect().bottom,
      height: window.innerHeight
    }));
    expect(dimensions.overflow).toBe(false);
    expect(dimensions.composer).toBeLessThanOrEqual(dimensions.height);
    await page.screenshot({ path: test.info().outputPath('layout.png') });
  });
}

test('long streaming output retains DOM nodes and respects manual scroll position', async ({ page }) => {
  snapshot.messages = Array.from({ length: 80 }, (_, i) => message(`m${i}`, i % 2 ? 'assistant' : 'user', 'A retained line.\n'.repeat(8)));
  await ready(page);
  await page.evaluate(() => { window.savedFirst = document.querySelector('.message'); document.querySelector('#conversation').scrollTop = 0; });
  await expect(page.locator('#jump')).toBeVisible();
  for (let i = 0; i < 200; i++) send('update', { type: 'delta', message_id: 'm79', text: 'new text '.repeat(20) });
  await expect(page.locator('.message-text').last()).toContainText('new text');
  expect(await page.evaluate(() => document.querySelector('.message') === window.savedFirst)).toBe(true);
  expect(await page.locator('.message').count()).toBe(80);
  expect(await page.locator('#conversation').evaluate(node => node.scrollTop)).toBeLessThan(100);
  await page.locator('#input').fill('Typing stays responsive');
  await expect(page.locator('#input')).toHaveValue('Typing stays responsive');
});
