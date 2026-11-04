import { defineConfig } from 'steiger';
// The plugin exports a conditional type that tsgolint cannot resolve yet.
// Canonical tsc checks this config; architecture checks still run unmodified.
import fsd from '@feature-sliced/steiger-plugin';
// oxlint-disable-next-line typescript/no-unsafe-assignment
export default defineConfig([...fsd.configs.recommended]);
