import { refractor } from 'refractor/core';
import type { MarkdownNode } from './contracts';
import { remainingNodeBudget } from './parse';
const grammars = {
  javascript: () => import('refractor/javascript'),
  typescript: () => import('refractor/typescript'),
  rust: () => import('refractor/rust'),
  python: () => import('refractor/python'),
  bash: () => import('refractor/bash'),
  json: () => import('refractor/json'),
  css: () => import('refractor/css'),
};
const aliases: Record<string, string> = {
  js: 'javascript',
  ts: 'typescript',
  sh: 'bash',
  py: 'python',
};
function language(value: string): keyof typeof grammars | null {
  const name = aliases[value] ?? value;
  switch (name) {
    case 'javascript':
    case 'typescript':
    case 'rust':
    case 'python':
    case 'bash':
    case 'json':
    case 'css':
      return name;
    default:
      return null;
  }
}
/** Highlighting returns nodes, never HTML, and loads only an allowlisted grammar. */
export async function highlight(
  nodes: MarkdownNode[],
  budget = { remaining: remainingNodeBudget(nodes) },
  depth = 0,
) {
  for (const node of nodes) {
    if (node.children) await highlight(node.children, budget, depth + 1);
    if (node.tag !== 'pre' || !node.text || node.text.length > 16000 || budget.remaining <= 0)
      continue;
    const name = language(node.language ?? '');
    if (!name) continue;
    const grammar = await grammars[name]();
    refractor.register(grammar.default);
    type HighlightNode = ReturnType<typeof refractor.highlight>['children'][number];
    const convert = (node: HighlightNode, level: number): MarkdownNode => {
      if (--budget.remaining < 0 || level > 48) throw new RangeError('Highlight node limit');
      if (node.type === 'text') return { tag: 'text', text: node.value };
      if (node.type !== 'element') return { tag: 'text', text: '' };
      return {
        tag: 'span',
        className: Array.isArray(node.properties.className)
          ? node.properties.className
              .filter((name): name is string => typeof name === 'string' && /^[a-z-]+$/.test(name))
              .join(' ')
          : '',
        children: node.children.map((child) => convert(child, level + 1)),
      };
    };
    try {
      node.children = refractor
        .highlight(node.text, name)
        .children.map((child) => convert(child, depth + 1));
    } catch (error: unknown) {
      // Keep the code card and literal code if syntax expansion is too complex.
      if (!(error instanceof RangeError)) throw error;
    }
  }
}
