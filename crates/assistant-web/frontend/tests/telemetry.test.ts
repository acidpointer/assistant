import { expect, it } from 'vitest';
import { tokensPerSecond, memory } from '../src/entities/telemetry';
it('uses weighted native counters, handles absent measurements and exact units', () => {
  expect(tokensPerSecond(100 + 10, 1e9 + 10e9)).toBe(10);
  expect(tokensPerSecond(0, 1e9)).toBe(0);
  expect(tokensPerSecond(1, 0)).toBeNull();
  expect(memory(null)).toBe('Unavailable');
  expect(memory(0)).toBe('0.0 MiB');
  expect(memory(1024 ** 3)).toBe('1.00 GiB');
});
