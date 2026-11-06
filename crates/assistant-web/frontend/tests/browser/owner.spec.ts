import { test, expect } from '@playwright/test';
import type { Page } from '@playwright/test';
import { fixture } from './fixture';

let host: Awaited<ReturnType<typeof fixture>>;
test.beforeEach(async () => {
  host = await fixture({ memory: true });
});
test.afterEach(async () => {
  await host?.close();
});
async function ready(page: Page) {
  await expect(page.locator('.statusline')).toContainText('Ready on your device');
}

test('memory opens directly and remains available after reload', async ({ page }) => {
  const errors: string[] = [];
  page.on('pageerror', (error) => errors.push(error.message));
  await page.goto(host.base);
  await ready(page);
  await expect(page.getByLabel('Message Assistant')).toBeVisible();
  await expect(page.getByLabel('Pairing code')).toHaveCount(0);
  await expect(page.getByRole('button', { name: 'Disconnect', exact: true })).toHaveCount(0);
  await page.getByRole('button', { name: 'Memory', exact: true }).click();
  await expect(page.getByLabel('Memory status')).toContainText('conversations and connected notes');
  await page.getByRole('button', { name: 'Browse saved history' }).click();
  await expect(page.getByLabel('Saved history')).toContainText('No eligible saved exchanges');
  await page.reload();
  await ready(page);
  await expect(page.getByLabel('Message Assistant')).toBeVisible();
  expect(errors).toEqual([]);
});

test('reload reconciles only the original opaque key and never resubmits a lost draft', async ({
  page,
}) => {
  await page.goto(host.base);
  await ready(page);
  await page.getByLabel('Message Assistant').fill('lost-both');
  await page.getByRole('button', { name: 'Send message' }).click();
  await expect(page.getByRole('alert')).toContainText('Acknowledgment lost');
  const identity = await page.evaluate(() => sessionStorage.getItem('assistant.pending'));
  expect(identity).toContain('"namespace":"web"');
  expect(identity).not.toContain('lost-both');
  const transmissions = host.submissions.length;
  await page.reload();
  await ready(page);
  await expect(page.getByRole('alert')).toContainText('original request is absent');
  expect(host.submissions).toHaveLength(transmissions);
  await expect(page.getByLabel('Message Assistant')).toHaveValue('');
});

test('a stream reconnect keeps the conversation usable without another action', async ({
  page,
}) => {
  await page.goto(host.base);
  await ready(page);
  await page.getByLabel('Message Assistant').fill('keep this draft across reconnect');
  host.reconnect();
  await expect(page.locator('.statusline')).toContainText('Ready on your device');
  await expect(page.getByLabel('Message Assistant')).toHaveValue(
    'keep this draft across reconnect',
  );
});

test('memory controls remain reachable in a narrow viewport', async ({ page }) => {
  host.snapshot.inference = { generated_tokens: 100, generation_ns: 1e9 };
  await page.setViewportSize({ width: 320, height: 640 });
  await page.goto(host.base);
  await ready(page);
  const button = page.getByRole('button', { name: 'Memory', exact: true });
  const box = await button.boundingBox();
  expect(box).not.toBeNull();
  expect((box?.x ?? 0) + (box?.width ?? 0)).toBeLessThanOrEqual(320);
  await page.getByRole('button', { name: 'Memory', exact: true }).click();
  await expect(page.getByRole('button', { name: 'Close', exact: true })).toBeInViewport();
  expect(await page.evaluate(() => document.documentElement.scrollWidth)).toBeLessThanOrEqual(320);
});

test('relationship inspection preserves original direction, qualifications and source role', async ({
  page,
}) => {
  const input = {
    subject: 'Oleh',
    predicate: 'parent_of',
    object: { kind: 'reference', value: 'owner' },
    negative: false,
    modality: 'reported',
    valid_from: null,
    valid_to: null,
    conditions: ['only within the authored scenario'],
    evidence: [
      { source_id: 'source', source_revision: '1', quote: 'Oleh was described as my father.' },
    ],
  };
  host.setGraph({
    sequence: '4',
    truncated: false,
    entities: [
      { id: 'owner', kind: 'person', label: 'owner' },
      { id: 'Oleh', kind: 'person', label: 'Oleh' },
    ],
    claims: [{ id: 'parent-claim', revision: '1', input }],
    sources: [{ source: { id: 'source', revision: '1' }, role: 'assistant', observed_at: null }],
    paths: [
      {
        claim: 'parent-claim',
        steps: [{ claim: 'parent-claim', revision: '1', from: 'owner', to: 'Oleh' }],
      },
    ],
  });
  await page.goto(host.base);
  await ready(page);
  await page.getByRole('button', { name: 'Memory', exact: true }).click();
  await page.getByText('Structured claims from earlier experiments', { exact: true }).click();
  await page.getByRole('button', { name: 'Inspect relationships' }).click();
  const region = page.getByLabel('Relationships', { exact: true });
  await expect(region).toContainText('Oleh · parent of · owner');
  await expect(region).toContainText('reported');
  await expect(region).toContainText('only within the authored scenario');
  await expect(region).toContainText('assistant · observed unknown');
  await region.getByText('Navigation provenance', { exact: true }).click();
  await expect(region).toContainText('owner → Oleh');
});

test('personality persists and an assistant can initiate dialogue without a user submission', async ({
  page,
}) => {
  await page.goto(host.base);
  await ready(page);
  await page.getByRole('button', { name: 'Assistant', exact: true }).click();
  const settings = page.getByLabel('Assistant settings', { exact: true });
  await settings
    .getByLabel('Personality and behavior')
    .fill('Be a playful anime companion; use clear prose for technical work.');
  await settings.getByLabel('Let my assistant start a conversation').check();
  await settings.getByLabel('Check at most every (minutes)').fill('1');
  await settings.getByRole('button', { name: 'Save assistant' }).click();
  await expect(settings).toContainText('Saved.');
  await settings.getByRole('button', { name: 'Close', exact: true }).click();
  await page.getByRole('button', { name: 'Assistant', exact: true }).click();
  await expect(settings.getByLabel('Personality and behavior')).toHaveValue(
    'Be a playful anime companion; use clear prose for technical work.',
  );
  await expect(settings.getByLabel('Let my assistant start a conversation')).toBeChecked();
  await settings.getByRole('button', { name: 'Close', exact: true }).click();
  host.initiate('How did your garden bench turn out?');
  await expect(page.getByLabel('Assistant started a conversation')).toContainText('garden bench');
  expect(host.submissions).toHaveLength(0);
  await page.getByRole('button', { name: 'Dismiss', exact: true }).click();
  await expect(page.getByLabel('Assistant started a conversation')).toHaveCount(0);
  expect(await page.evaluate(() => Object.values(sessionStorage).join(' '))).not.toContain(
    'playful anime',
  );
});
