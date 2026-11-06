/** Real-daemon browser smoke. Open the local assistant directly.
 * The Python owner creates/joins the daemon and owns only synthetic test data. */
import { chromium } from '@playwright/test';
import assert from 'node:assert/strict';
import * as v from 'valibot';

let input = '';
for await (const chunk of process.stdin) {
  input += chunk;
  assert.ok(input.length <= 4096, 'bounded smoke configuration');
}
/** @type {unknown} */
const payload = JSON.parse(input);
const { url, expectedInput, screenshot } = v.parse(
  v.object({
    url: v.string(),
    expectedInput: v.string(),
    screenshot: v.nullable(v.string()),
  }),
  payload,
);
assert.ok(['127.0.0.1', '[::1]'].includes(new URL(url).hostname));
const browser = await chromium.launch();
try {
  const context = await browser.newContext({ viewport: { width: 1000, height: 800 } });
  const page = await context.newPage();
  let pageErrors = 0;
  page.on('pageerror', () => {
    pageErrors += 1;
  });
  await page.goto(url);
  await page.locator('.statusline').filter({ hasText: 'Ready on your device' }).waitFor();
  await page.locator('.user-text').filter({ hasText: expectedInput }).waitFor();
  await page.getByRole('button', { name: 'Assistant', exact: true }).click();
  const settings = page.getByLabel('Assistant settings', { exact: true });
  await settings.getByLabel('Personality and behavior').waitFor();
  assert.ok(
    (await settings.getByLabel('Personality and behavior').inputValue()).includes(
      'project companion',
    ),
  );
  await settings.getByRole('button', { name: 'Close', exact: true }).click();
  await page.getByRole('button', { name: 'Memory', exact: true }).click();
  await page.getByLabel('Memory status').filter({ hasText: 'Semantic coverage:' }).waitFor();
  await page
    .getByLabel('Note to remember')
    .fill('For bench cut lists, ask me about dimensions first.');
  await page.getByRole('button', { name: 'Save note', exact: true }).click();
  await page.getByLabel('Memory', { exact: true }).filter({ hasText: 'Note saved.' }).waitFor();
  await page.getByLabel('Find a remembered source').fill('bicycle');
  await page.getByRole('button', { name: 'Search', exact: true }).click();
  await page.locator('.memory-source').filter({ hasText: 'bicycle is blue' }).first().waitFor();
  await page.getByRole('button', { name: 'Explore connections', exact: true }).first().click();
  const connected = page.getByLabel('Connected memory');
  await connected.filter({ hasText: 'bicycle' }).waitFor();
  assert.ok(
    (await connected.getByRole('button', { name: 'Continue from this source' }).count()) >= 2,
    'a search hit exposes connected experiences',
  );
  await page.getByRole('button', { name: 'Browse saved history' }).click();
  await page.getByLabel('Saved history').filter({ hasText: expectedInput }).waitFor();
  if (screenshot) await page.screenshot({ path: screenshot });
  await page.setViewportSize({ width: 320, height: 640 });
  assert.ok(
    await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth),
    'memory UI fits a 320px viewport',
  );
  await page.reload();
  await page.locator('.statusline').filter({ hasText: 'Ready on your device' }).waitFor();
  assert.equal(await page.getByLabel('Pairing code').count(), 0);
  assert.equal(await page.getByLabel('Message Assistant').count(), 1);
  const storage = await context.storageState();
  assert.deepEqual(storage.cookies, []);
  assert.ok(storage.origins.every((origin) => origin.localStorage.length === 0));
  assert.equal(pageErrors, 0);
  await context.close();
  console.log(
    'PASS: real-daemon direct browser access, durable snapshot, memory search/history, mobile layout and reload',
  );
} finally {
  await browser.close();
}
