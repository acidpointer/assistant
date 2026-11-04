import { readFile } from 'node:fs/promises';
import { expect, it } from 'vitest';
import * as v from 'valibot';
import { chatSnapshotSchema } from '../src/entities/chat';
import { resourceSchema, totalsSchema } from '../src/entities/telemetry';

const path = process.env['ASSISTANT_TEST_CONTRACT'];
it.skipIf(!path)(
  'decodes an actual Rust daemon snapshot including native measurements',
  async () => {
    if (!path) throw new Error('Missing contract artifact');
    const raw: unknown = JSON.parse(await readFile(path, 'utf8'));
    const snapshot = v.parse(
      v.intersect([
        chatSnapshotSchema,
        v.object({ resources: v.nullable(resourceSchema), inference: totalsSchema }),
      ]),
      raw,
    );
    expect(
      snapshot.recent_runs.some((run) =>
        run.performance.some((turn) => turn.measurement_complete && turn.generated_tokens > 0),
      ),
    ).toBe(true);
  },
);
