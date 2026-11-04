import { Lexer } from 'marked';
import type { Token, MarkedToken } from 'marked';
import { parseEntities } from 'parse-entities';
import type { MarkdownNode } from './contracts';

const knownTypes = new Set([
  'blockquote',
  'br',
  'checkbox',
  'code',
  'codespan',
  'def',
  'del',
  'em',
  'escape',
  'heading',
  'hr',
  'html',
  'image',
  'link',
  'list',
  'list_item',
  'paragraph',
  'space',
  'strong',
  'table',
  'text',
]);
/** This lexer has no extensions: known token names have Marked's built-in shape.
 * Arbitrary JSON is never passed here. No token text is interpreted as HTML. */
function builtin(token: Token): token is MarkedToken {
  return knownTypes.has(token.type);
}
export function safeUrl(value: string): string | null {
  const decoded = parseEntities(value).trim();
  if (
    Array.from(decoded).some(
      (character) => character.charCodeAt(0) <= 32 || character.charCodeAt(0) === 127,
    )
  )
    return null;
  try {
    const url = new URL(decoded);
    return ['http:', 'https:', 'mailto:'].includes(url.protocol) ? url.href : null;
  } catch {
    return null;
  }
}
/** Normalize GFM into a closed element vocabulary. Raw HTML becomes literal text;
 * images remain alt text so a model cannot trigger background network requests. */
export function parseMarkdown(text: string): MarkdownNode[] {
  if (new TextEncoder().encode(text).length > 131072) throw new Error('Markdown input limit');
  const lexer = new Lexer({ gfm: true });
  let remaining = 12000;
  function convert(tokens: readonly Token[], depth = 0): MarkdownNode[] {
    if (depth > 48) throw new Error('Markdown nesting limit');
    return tokens.filter(builtin).flatMap((token): MarkdownNode[] => {
      if (--remaining < 0) throw new Error('Markdown node limit');
      const children = (nested: readonly Token[]) => convert(nested, depth + 1);
      switch (token.type) {
        case 'space':
        case 'def':
          return [];
        case 'text':
          return token.tokens
            ? children(token.tokens)
            : [{ tag: 'text', text: parseEntities(token.text) }];
        case 'escape':
          return [{ tag: 'text', text: parseEntities(token.text) }];
        case 'html':
          return [{ tag: 'text', text: token.text }];
        case 'image':
          return [{ tag: 'text', text: `[Image: ${parseEntities(token.text)}]` }];
        case 'codespan':
          return [{ tag: 'code', text: token.text }];
        case 'code':
          return [{ tag: 'pre', text: token.text, language: token.lang?.split(/\s+/)[0] ?? '' }];
        case 'hr':
        case 'br':
          return [{ tag: token.type }];
        case 'heading': {
          const tags = ['h1', 'h2', 'h3', 'h4', 'h5', 'h6'] as const;
          return [{ tag: tags[token.depth - 1] ?? 'h6', children: children(token.tokens) }];
        }
        case 'strong':
        case 'em':
        case 'del':
        case 'blockquote':
          return [{ tag: token.type, children: children(token.tokens) }];
        case 'paragraph':
          return [{ tag: 'p', children: children(token.tokens) }];
        case 'link': {
          const href = safeUrl(token.href);
          return href
            ? [{ tag: 'a', href, children: children(token.tokens) }]
            : children(token.tokens);
        }
        case 'checkbox':
          return [{ tag: 'input', checked: token.checked }];
        case 'list':
          return [
            {
              tag: token.ordered ? 'ol' : 'ul',
              start: token.start === '' ? 1 : token.start,
              children: token.items.map((item) => ({
                tag: 'li',
                children: [
                  ...(item.task ? [{ tag: 'input' as const, checked: item.checked ?? false }] : []),
                  ...children(item.tokens),
                ],
              })),
            },
          ];
        case 'list_item':
          return [{ tag: 'li', children: children(token.tokens) }];
        case 'table':
          return [
            {
              tag: 'table',
              children: [
                {
                  tag: 'thead',
                  children: [
                    {
                      tag: 'tr',
                      children: token.header.map((cell) => ({
                        tag: 'th',
                        children: children(cell.tokens),
                      })),
                    },
                  ],
                },
                {
                  tag: 'tbody',
                  children: token.rows.map((row) => ({
                    tag: 'tr',
                    children: row.map((cell) => ({ tag: 'td', children: children(cell.tokens) })),
                  })),
                },
              ],
            },
          ];
      }
    });
  }
  const nodes = convert(lexer.lex(text));
  remainingNodeBudget(nodes);
  return nodes;
}

/** Count actual render nodes, including table/list wrappers absent from tokens. */
export function remainingNodeBudget(nodes: readonly MarkdownNode[]): number {
  let remaining = 12000;
  function visit(children: readonly MarkdownNode[], depth: number) {
    if (depth > 48) throw new RangeError('Markdown nesting limit');
    for (const node of children) {
      if (--remaining < 0) throw new RangeError('Markdown node limit');
      if (node.children) visit(node.children, depth + 1);
    }
  }
  visit(nodes, 0);
  return remaining;
}
