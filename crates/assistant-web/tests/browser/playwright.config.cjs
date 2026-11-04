/** Current-engine regression coverage; oldest embedded engines need device checks. */
const { defineConfig } = require('@playwright/test');
module.exports = defineConfig({
  testDir: __dirname,
  testMatch: 'webui.spec.cjs',
  timeout: 30000,
  fullyParallel: false,
  workers: 1,
  use: { viewport: { width: 1000, height: 800 } },
  projects: [
    { name: 'chromium', use: { browserName: 'chromium' } },
    { name: 'firefox', use: { browserName: 'firefox' } },
    { name: 'webkit', use: { browserName: 'webkit' } }
  ]
});
