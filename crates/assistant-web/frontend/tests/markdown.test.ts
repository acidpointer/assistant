import { describe, expect, it } from 'vitest';
import { parseMarkdown, safeUrl } from '../src/shared/ui/markdown/parse';
import { highlight } from '../src/shared/ui/markdown/highlight';
describe('untrusted Markdown', () => {
  it('renders GFM without executable elements or automatic image requests', () => {
    const tree = parseMarkdown(
      '# Heading\n\n| a | b |\n|---|---|\n| 1 | 2 |\n\n- [x] done\n\n<script>alert(1)</script>\n\n![secret](https://evil.invalid/x)\n\n[run](javascript:alert%281%29)',
    );
    const json = JSON.stringify(tree);
    expect(json).toContain('"tag":"table"');
    expect(json).toContain('"checked":true');
    expect(parseMarkdown('0. Zero')[0]?.start).toBe(0);
    expect(json).toContain('<script>');
    expect(json).not.toContain('"tag":"script"');
    expect(json).not.toContain('"tag":"img"');
    expect(json).not.toContain('"href":"javascript:');
  });
  it('rejects encoded and whitespace-obfuscated executable URLs', () => {
    for (const url of [
      'javascript:alert(1)',
      'java&#x73;cript:alert(1)',
      'data:text/html,<svg>',
      'java\nscript:x',
      'file:///etc/passwd',
    ])
      expect(safeUrl(url)).toBeNull();
    expect(safeUrl('https://example.com/a')).toBe('https://example.com/a');
  });
  it('revises earlier reference links when their definition arrives', () => {
    const prefix = '[reference][id]\n\n```rust\nfn main() {}';
    expect(JSON.stringify(parseMarkdown(prefix))).not.toContain('"href"');
    const final = parseMarkdown(prefix + '\n```\n\n[id]: https://example.com\n');
    expect(JSON.stringify(final)).toContain('https://example.com/');
    expect(final.some((node) => node.tag === 'pre')).toBe(true);
  });
  it('highlights only a fixed grammar set into safe span/text nodes', async () => {
    const nodes = parseMarkdown('```rust\nfn main() {}\n```');
    await highlight(nodes);
    expect(JSON.stringify(nodes)).toContain('keyword');
    expect(nodes[0]?.text).toBe('fn main() {}');
  });
  it('bounds input and structural work', () => {
    expect(() => parseMarkdown('x'.repeat(131073))).toThrow('input limit');
    expect(() => parseMarkdown('> '.repeat(60) + 'nested')).toThrow('nesting limit');
  });
});

it('keeps dense code literal when syntax spans would exceed the render budget', async () => {
  const code = '{} '.repeat(4000);
  const nodes = parseMarkdown('```rust\n' + code + '\n```');
  await highlight(nodes, { remaining: 32 });
  expect(nodes[0]?.text).toBe(code);
  expect(nodes[0]?.children).toBeUndefined();
});
