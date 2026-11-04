/// <reference lib="webworker" />
import * as v from 'valibot';
import { jobSchema } from './contracts';
import type { MarkdownResult } from './contracts';
import { parseMarkdown } from './parse';
import { highlight } from './highlight';
self.onmessage = (event: MessageEvent<unknown>) => {
  void (async () => {
    const job = v.parse(jobSchema, event.data);
    let result: MarkdownResult;
    try {
      const nodes = parseMarkdown(job.text);
      // A changing code fence stays plain until completion; avoid running syntax
      // grammars on every token and preserve typing responsiveness on small CPUs.
      if (job.complete) await highlight(nodes);
      result = {
        id: job.id,
        revision: job.revision,
        error: null,
        blocks: nodes.map((node) => ({ node, signature: JSON.stringify(node) })),
      };
    } catch {
      result = {
        id: job.id,
        revision: job.revision,
        blocks: [],
        error: 'Markdown complexity limit. Showing source.',
      };
    }
    self.postMessage(result);
  })();
};
