import { defineConfig } from '@playwright/test';
const webkitExecutable = process.env['ASSISTANT_TEST_WEBKIT_EXECUTABLE'];
export default defineConfig({
  testDir: './tests/browser',
  reporter: [['list'], ['json', { outputFile: 'test-results/results.json' }]],
  timeout: 30000,
  workers: 1,
  fullyParallel: false,
  use: { viewport: { width: 1000, height: 800 } },
  projects: [
    { name: 'chromium', use: { browserName: 'chromium' } },
    { name: 'firefox', use: { browserName: 'firefox' } },
    {
      name: 'webkit',
      use: {
        browserName: 'webkit',
        ...(webkitExecutable ? { launchOptions: { executablePath: webkitExecutable } } : {}),
      },
    },
  ],
});
