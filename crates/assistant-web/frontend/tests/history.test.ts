import { expect, it } from 'vitest';
import { PromptHistory } from '../src/pages/chat/model/history';
it('recalls accepted prompts while preserving normal drafts and limiting history', () => {
  const history = new PromptHistory();
  for (let index = 0; index < 70; index++) history.accept(`prompt ${index}`);
  expect(history.previous('a multiline\ndraft')).toBeNull();
  expect(history.previous('')).toBe('prompt 69');
  expect(history.previous('prompt 69')).toBe('prompt 68');
  expect(history.next()).toBe('prompt 69');
  expect(history.next()).toBe('');
  for (let index = 0; index < 100; index++) history.previous('');
  expect(history.previous('')).toBe('prompt 6');
  expect(history.restore()).toBe('');
  expect(history.browsing).toBe(false);
});
