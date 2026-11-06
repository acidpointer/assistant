import { afterEach, expect, test, vi } from 'vitest';
import { configureApi, request, response } from '../src/shared/api/http';

afterEach(() => {
  vi.unstubAllGlobals();
  configureApi(1);
});

test('memory requests connect directly without credentials', async () => {
  const fetch = vi.fn().mockResolvedValue(new Response('{}'));
  vi.stubGlobal('fetch', fetch);
  configureApi(2);
  await request('/snapshot');
  expect(fetch.mock.calls[0]?.[0]).toBe('/api/v2/snapshot');
  const options = fetch.mock.calls[0]?.[1] as RequestInit;
  expect(new Headers(options.headers).has('Authorization')).toBe(false);
  expect(options.credentials).toBe('omit');
});

test('HTTP failures preserve the daemon error code', async () => {
  vi.stubGlobal(
    'fetch',
    vi.fn().mockResolvedValue(new Response('{"code":"conflict"}', { status: 409 })),
  );
  await expect(response('/runs')).rejects.toMatchObject({ code: 'conflict', status: 409 });
});
