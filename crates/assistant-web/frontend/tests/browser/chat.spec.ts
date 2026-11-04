import { test, expect } from '@playwright/test';
import type { Page } from '@playwright/test';
import { fixture, message, sample } from './fixture';
declare global {
  interface Window {
    inputLatencies: number[];
  }
}
let host: Awaited<ReturnType<typeof fixture>>;
test.beforeEach(async () => {
  host = await fixture();
});
test.afterEach(async () => {
  await host.close();
});
async function ready(page: Page) {
  await page.goto(host.base);
  await expect(page.locator('.statusline')).toContainText('Ready on your device');
}
test('safe streaming Markdown, stop, metrics and draft preservation', async ({ page }) => {
  host.snapshot.inference = { generated_tokens: 100, generation_ns: 1e9 };
  const errors: string[] = [];
  page.on('pageerror', (error) => errors.push(error.message));
  await ready(page);
  await page.getByLabel('Message Assistant').fill('Hello');
  await page.getByRole('button', { name: 'Send message' }).click();
  await expect(page.getByRole('button', { name: 'Stop', exact: false })).toBeVisible();
  await expect(page.locator('.overall-metrics summary')).toContainText('— tok/s');
  await expect(page.getByLabel('Message Assistant')).toHaveValue('');
  await page.getByLabel('Message Assistant').fill('Next question');
  const text =
    '# Useful answer\n\n**Strong** and [unsafe](javascript:alert%281%29)\n\n<img src=x onerror=alert(1)>\n\n```rust\nfn main() {}\n```';
  host.send('update', { type: 'delta', message_id: 'run1-assistant-1', text });
  await expect(page.locator('.markdown h1')).toHaveText('Useful answer');
  await expect(page.locator('.markdown strong')).toHaveText('Strong');
  await expect(page.locator('.markdown img')).toHaveCount(0);
  await expect(page.locator('.markdown a')).toHaveCount(0);
  host.complete(text);
  await expect(page.locator('.message-metrics summary')).toContainText('50.0 tok/s');
  await expect(page.locator('.overall-metrics summary')).toContainText('75.0 tok/s');
  await expect(page.locator('.message-metrics summary')).toContainText('2.00 GiB');
  await expect(page.locator('.token.keyword')).toHaveText('fn');
  await page.locator('.message-metrics summary').click();
  await expect(page.locator('.message-metric-details')).toContainText('125.0%');
  await page.getByLabel('Performance details').click();
  await expect(page.locator('.metrics-popover')).toContainText('Fixture GPU');
  await expect(page.locator('.metrics-popover')).toContainText('0000:25:00.0');
  await expect(page.locator('.metrics-popover')).toContainText('2.00 GiB');
  await expect(page.getByLabel('Message Assistant')).toHaveValue('Next question');
  expect(errors).toEqual([]);
});
test('IME, multiline navigation, recall, Escape and quick actions share normal input behavior', async ({
  page,
}) => {
  await ready(page);
  const input = page.getByLabel('Message Assistant');
  await input.fill('First prompt');
  await input.dispatchEvent('keydown', { key: 'Enter', isComposing: true, keyCode: 229 });
  expect(host.submissions).toHaveLength(0);
  await input.press('Shift+Enter');
  await expect(input).toHaveValue('First prompt\n');
  await input.fill('First prompt');
  await input.press('Enter');
  await expect(input).toHaveValue('');
  await input.press('Escape');
  await expect(page.getByRole('button', { name: 'Send message' })).toBeVisible();
  await input.press('ArrowUp');
  await expect(input).toHaveValue('First prompt');
  await input.press('ArrowDown');
  await expect(input).toHaveValue('');
  await input.fill('line one\nline two');
  await input.press('ArrowUp');
  await expect(input).toHaveValue('line one\nline two');
  await page.keyboard.press('Control+k');
  await expect(page.getByRole('dialog')).toBeVisible();
  await page.keyboard.press('Escape');
  await expect(page.getByRole('dialog')).not.toBeVisible();
  expect(host.submissions).toHaveLength(1);
});
for (const input of ['finish-before-ack', 'drop-ack'])
  test(`${input} cannot duplicate input or revive a finished run`, async ({ page }) => {
    await ready(page);
    await page.getByLabel('Message Assistant').fill(input);
    await page.getByLabel('Message Assistant').press('Enter');
    await expect(page.getByLabel('Message Assistant')).toHaveValue('');
    if (input === 'finish-before-ack') {
      await expect(page.locator('.markdown')).toHaveText('Already complete');
      await expect(page.getByRole('button', { name: 'Send message' })).toBeVisible();
    } else await expect(page.getByRole('button', { name: 'Stop', exact: false })).toBeVisible();
    expect(host.submissions.length).toBeGreaterThanOrEqual(1);
    const first = host.submissions[0];
    expect(
      host.submissions.every(
        (request) =>
          request.client_id === first?.client_id &&
          request.client_request_id === first.client_request_id,
      ),
    ).toBe(true);
    expect(host.snapshot.recent_runs).toHaveLength(1);
    await expect(page.getByRole('article', { name: 'user message' })).toHaveCount(1);
  });
test('wall clock changes preserve ambiguous request keys', async ({ page }) => {
  await ready(page);
  await page.getByLabel('Message Assistant').fill('lost-both');
  await page.getByRole('button', { name: 'Send message' }).click();
  await expect(page.getByRole('alert')).toContainText('Acknowledgment lost');
  const key = host.submissions[0]?.client_request_id;
  await page.evaluate(() => {
    const now = Date.now();
    Date.now = () => now + 3600000;
  });
  await page.getByRole('button', { name: 'Send message' }).click();
  // Browsers may retry a dropped HTTP connection once. Every transmission must
  // retain the key; the daemon, rather than a browser retry count, owns dedup.
  await expect.poll(() => host.submissions.length).toBeGreaterThanOrEqual(2);
  expect(host.submissions.every((input) => input.client_request_id === key)).toBe(true);
});
test('reconnect and invalid deltas replace snapshots while keeping drafts', async ({ page }) => {
  await ready(page);
  await page.getByLabel('Message Assistant').fill('Keep this draft');
  host.snapshot.messages = [message('external-assistant-1', 'assistant', 'Arrived elsewhere')];
  host.reconnect();
  await expect(page.locator('.markdown')).toHaveText('Arrived elsewhere', { timeout: 15000 });
  host.snapshot.messages = [message('recovered-assistant-1', 'assistant', 'Recovered state')];
  host.send('update', { type: 'delta', message_id: 'missing', text: 'invalid' });
  await expect(page.locator('.markdown')).toHaveText('Recovered state');
  await expect(page.getByLabel('Message Assistant')).toHaveValue('Keep this draft');
  expect(host.submissions).toHaveLength(0);
});
for (const width of [320, 768, 2560])
  test(`responsive layout at ${width}px`, async ({ page }) => {
    await page.setViewportSize({ width, height: 640 });
    host.snapshot.messages = [message('long-assistant-1', 'assistant', 'unbroken'.repeat(1000))];
    await ready(page);
    const dimensions = await page.evaluate(() => ({
      overflow: document.documentElement.scrollWidth > innerWidth,
      composer: document.getElementById('prompt')?.getBoundingClientRect().bottom ?? Infinity,
      height: innerHeight,
    }));
    expect(dimensions.overflow).toBe(false);
    expect(dimensions.composer).toBeLessThanOrEqual(dimensions.height);
    await page.screenshot({ path: test.info().outputPath('layout.png') });
  });
test('long streams preserve nodes, manual scroll and input latency', async ({ page }) => {
  host.snapshot.messages = Array.from({ length: 80 }, (_, i) =>
    message(
      `run${i}-assistant-1`,
      i % 2 ? 'assistant' : 'user',
      'A retained paragraph.\n\n'.repeat(4),
    ),
  );
  await ready(page);
  const first = await page.locator('.message').first().elementHandle();
  await page.locator('.transcript').evaluate((node) => {
    node.scrollTop = 0;
  });
  await expect(page.getByRole('button', { name: '↓ Latest' })).toBeVisible();
  for (let i = 0; i < 200; i++)
    host.send('update', {
      type: 'delta',
      message_id: 'run79-assistant-1',
      text: 'new text '.repeat(20),
    });
  await expect(page.locator('.markdown').last()).toContainText('new text');
  expect(await first?.evaluate((node) => node === document.querySelector('.message'))).toBe(true);
  expect(await page.locator('.transcript').evaluate((node) => node.scrollTop)).toBeLessThan(100);
  await page.evaluate(() => {
    window.inputLatencies = [];
    document.getElementById('prompt')?.addEventListener('input', () => {
      const start = performance.now();
      requestAnimationFrame(() => window.inputLatencies.push(performance.now() - start));
    });
  });
  const streaming = setInterval(
    () =>
      host.send('update', {
        type: 'delta',
        message_id: 'run79-assistant-1',
        text: 'more text ',
      }),
    10,
  );
  try {
    await page.getByLabel('Message Assistant').focus();
    await page.keyboard.type('Typing stays responsive', { delay: 8 });
    await expect(page.getByLabel('Message Assistant')).toHaveValue('Typing stays responsive');
    await page.waitForFunction(() => window.inputLatencies.length > 10);
    const latencies = await page.evaluate(() => window.inputLatencies);
    latencies.sort((a, b) => a - b);
    const p95 = latencies[Math.floor(latencies.length * 0.95)] ?? Infinity;
    // Current-engine regression guard; minimum-device certification is separate.
    expect(p95).toBeLessThan(100);
    await test.info().attach('input-latency', {
      body: JSON.stringify({ p95_ms: p95, samples_ms: latencies }),
      contentType: 'application/json',
    });
  } finally {
    clearInterval(streaming);
  }
});
test('unsupported GPU measurements stay unavailable and reduced motion disables animation', async ({
  page,
}) => {
  host.snapshot.resources = {
    ...sample,
    vram_bytes: null,
    vram_source: null,
    vram_devices:
      sample.vram_devices?.map((device) => ({ ...device, bytes: null, source: null })) ?? null,
  };
  await page.emulateMedia({ reducedMotion: 'reduce' });
  await ready(page);
  await expect(page.locator('.overall-metrics summary')).toContainText('Unavailable');
  expect(
    await page.locator('.welcome').evaluate((node) => getComputedStyle(node).animationName),
  ).toBe('none');
  await page.screenshot({ path: test.info().outputPath('welcome.png') });
});
