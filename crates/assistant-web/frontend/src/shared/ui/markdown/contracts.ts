import * as v from 'valibot';
export interface MarkdownNode {
  tag:
    | 'text'
    | 'p'
    | 'h1'
    | 'h2'
    | 'h3'
    | 'h4'
    | 'h5'
    | 'h6'
    | 'strong'
    | 'em'
    | 'del'
    | 'blockquote'
    | 'ul'
    | 'ol'
    | 'li'
    | 'code'
    | 'pre'
    | 'hr'
    | 'br'
    | 'a'
    | 'table'
    | 'thead'
    | 'tbody'
    | 'tr'
    | 'th'
    | 'td'
    | 'span'
    | 'input';
  text?: string;
  children?: MarkdownNode[];
  href?: string;
  language?: string;
  start?: number;
  checked?: boolean;
  className?: string;
}
const nodeSchema: v.GenericSchema<MarkdownNode> = v.lazy(() =>
  v.object({
    tag: v.picklist([
      'text',
      'p',
      'h1',
      'h2',
      'h3',
      'h4',
      'h5',
      'h6',
      'strong',
      'em',
      'del',
      'blockquote',
      'ul',
      'ol',
      'li',
      'code',
      'pre',
      'hr',
      'br',
      'a',
      'table',
      'thead',
      'tbody',
      'tr',
      'th',
      'td',
      'span',
      'input',
    ]),
    text: v.exactOptional(v.string()),
    children: v.exactOptional(v.array(nodeSchema)),
    href: v.exactOptional(v.string()),
    language: v.exactOptional(v.string()),
    start: v.exactOptional(v.number()),
    checked: v.exactOptional(v.boolean()),
    className: v.exactOptional(v.string()),
  }),
);
export const resultSchema = v.object({
  id: v.string(),
  revision: v.number(),
  blocks: v.array(v.object({ node: nodeSchema, signature: v.string() })),
  error: v.nullable(v.string()),
});
export type MarkdownResult = v.InferOutput<typeof resultSchema>;
export const jobSchema = v.object({
  id: v.string(),
  revision: v.number(),
  text: v.string(),
  complete: v.boolean(),
});
export type MarkdownJob = v.InferOutput<typeof jobSchema>;
