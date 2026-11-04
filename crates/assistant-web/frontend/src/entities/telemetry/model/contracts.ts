import * as v from 'valibot';
const counter = v.pipe(v.number(), v.finite(), v.minValue(0));
const optionalCounter = v.nullable(counter);
const gpuSchema = v.object({
  name: v.pipe(v.string(), v.maxLength(128)),
  pci_bus_id: v.nullable(v.string()),
  bytes: optionalCounter,
  source: v.nullable(v.string()),
});
export const resourceSchema = v.object({
  sampled_at_unix_ms: optionalCounter,
  monotonic_ns: counter,
  process_cpu_ns: optionalCounter,
  cpu_percent: optionalCounter,
  ram_rss_bytes: optionalCounter,
  ram_pss_bytes: optionalCounter,
  vram_bytes: optionalCounter,
  vram_devices: v.nullable(v.pipe(v.array(gpuSchema), v.maxLength(32))),
  vram_source: v.nullable(v.string()),
  ram_source: v.nullable(v.string()),
});
const boundarySchema = v.object({ monotonic_ns: counter, process_cpu_ns: optionalCounter });
export const windowSchema = v.object({
  started: v.nullable(boundarySchema),
  ended: v.nullable(boundarySchema),
  before: v.nullable(resourceSchema),
  after: v.nullable(resourceSchema),
  max_observed_rss_bytes: optionalCounter,
  max_observed_vram_bytes: optionalCounter,
});
export const timingSchema = v.object({
  prefill_ns: counter,
  generation_ns: counter,
  elapsed_ns: counter,
  first_token_ns: optionalCounter,
});
export const performanceSchema = v.object({
  measurement_complete: v.boolean(),
  turn: counter,
  generated_tokens: counter,
  timing: v.nullable(timingSchema),
  resources: windowSchema,
});
export const totalsSchema = v.object({ generated_tokens: counter, generation_ns: counter });
export type ResourceSample = v.InferOutput<typeof resourceSchema>;
export type ResourceWindow = v.InferOutput<typeof windowSchema>;
export type Performance = v.InferOutput<typeof performanceSchema>;
export type Totals = v.InferOutput<typeof totalsSchema>;
