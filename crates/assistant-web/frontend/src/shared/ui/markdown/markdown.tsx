import { createElement, memo, useEffect, useRef, useState } from 'react';
import type { ReactNode } from 'react';
import type { MarkdownNode, MarkdownResult } from './contracts';
import { scheduleMarkdown } from './scheduler';
import { Button } from '../button';
function CodeBlock({ node }: { node: MarkdownNode }) {
  const [wrap, setWrap] = useState(false);
  const [copied, setCopied] = useState(false);
  const [copyError, setCopyError] = useState(false);
  async function copy() {
    try {
      await navigator.clipboard.writeText(node.text ?? '');
      setCopied(true);
      setCopyError(false);
    } catch {
      setCopyError(true);
    }
  }
  return (
    <div className={`code-card ${wrap ? 'wrap-code' : ''}`}>
      <div className="code-toolbar">
        <span>{node.language || 'text'}</span>
        <div>
          <Button aria-pressed={wrap} onClick={() => setWrap(!wrap)}>
            Wrap
          </Button>
          <Button
            onClick={() => {
              void copy();
            }}
          >
            {copied ? 'Copied' : 'Copy'}
          </Button>
        </div>
      </div>
      <pre>
        <code>{node.children ? renderNodes(node.children) : node.text}</code>
      </pre>
      {copyError && <output>Clipboard unavailable. Select and copy the code.</output>}
    </div>
  );
}
function renderNodes(nodes: readonly MarkdownNode[]): ReactNode {
  return nodes.map((node, index) => {
    if (node.tag === 'text') return node.text;
    if (node.tag === 'pre') return <CodeBlock key={index} node={node} />;
    if (node.tag === 'input')
      return (
        <input
          key={index}
          type="checkbox"
          checked={node.checked ?? false}
          disabled
          aria-label="Task status"
        />
      );
    if (node.tag === 'table')
      return (
        <div key={index} className="table-scroll">
          <table>{renderNodes(node.children ?? [])}</table>
        </div>
      );
    return createElement(
      node.tag,
      {
        key: index,
        ...(node.tag === 'a'
          ? { href: node.href, target: '_blank', rel: 'noopener noreferrer' }
          : {}),
        ...(node.tag === 'ol' ? { start: node.start } : {}),
        ...(node.tag === 'span' ? { className: node.className } : {}),
      },
      node.children ? renderNodes(node.children) : node.text,
    );
  });
}
const Block = memo(
  function Block({ node }: { node: MarkdownNode; signature: string }) {
    return renderNodes([node]);
  },
  (previous, next) => previous.signature === next.signature,
);
/** Completed messages keep their parsed blocks; only a changed message schedules
 * worker work. Plain source remains available if complexity or timing bounds fail. */
export const Markdown = memo(function Markdown({
  id,
  text,
  complete,
}: {
  id: string;
  text: string;
  complete: boolean;
}) {
  const revision = useRef(0);
  const [result, setResult] = useState<MarkdownResult | null>(null);
  useEffect(() => {
    const current = ++revision.current;
    return scheduleMarkdown({ id, revision: current, text, complete }, (next) => {
      if (revision.current === current) setResult(next);
    });
  }, [id, text, complete]);
  if (!result || result.error)
    return (
      <div className="markdown source-text">
        {text}
        {result?.error && <output>{result.error}</output>}
      </div>
    );
  return (
    <div className="markdown">
      {result.blocks.map((block, index) => (
        <Block key={index} {...block} />
      ))}
    </div>
  );
});
