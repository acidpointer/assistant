import * as v from 'valibot';
import { resultSchema } from './contracts';
import type { MarkdownJob, MarkdownResult } from './contracts';
interface Task {
  job: MarkdownJob;
  receive: (result: MarkdownResult) => void;
}
const pending = new Map<string, Task>();
let worker: Worker | null = null;
let active: Task | null = null;
let timer: ReturnType<typeof setTimeout> | undefined;
let watchdog: ReturnType<typeof setTimeout> | undefined;
function defer(delay: number) {
  if (timer === undefined)
    timer = setTimeout(() => {
      timer = undefined;
      pump();
    }, delay);
}
function pump() {
  if (active || pending.size === 0) return;
  const task = pending.values().next().value;
  if (!task) return;
  pending.delete(task.job.id);
  active = task;
  if (!worker) {
    worker = new Worker(new URL('./worker.ts', import.meta.url), { type: 'module' });
    worker.onmessage = (event: MessageEvent<unknown>) => {
      const result = v.safeParse(resultSchema, event.data);
      finish(result.success ? result.output : null);
    };
    worker.onerror = () => finish(null);
  }
  // A pathological grammar/lexer cannot monopolize the worker indefinitely.
  watchdog = setTimeout(() => finish(null), 1500);
  worker.postMessage(task.job);
}
function finish(result: MarkdownResult | null) {
  clearTimeout(watchdog);
  const task = active;
  active = null;
  if (!result) {
    worker?.terminate();
    worker = null;
  }
  if (task && !pending.has(task.job.id))
    task.receive(
      result ?? {
        id: task.job.id,
        revision: task.job.revision,
        blocks: [],
        error: 'Markdown timed out. Showing source.',
      },
    );
  defer(30);
}
/** One in-flight parse and one latest revision per mounted message. Full parsing
 * allows reference links to change earlier blocks; stable signatures preserve DOM. */
export function scheduleMarkdown(job: MarkdownJob, receive: Task['receive']) {
  pending.set(job.id, { job, receive });
  defer(job.complete ? 0 : 60);
  return () => {
    if (pending.get(job.id)?.job.revision === job.revision) pending.delete(job.id);
    if (active?.job.id === job.id && active.job.revision === job.revision)
      active.receive = () => {};
  };
}
