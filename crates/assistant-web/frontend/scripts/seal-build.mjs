import { readdir, readFile, writeFile } from 'node:fs/promises';
import { gzipSync } from 'node:zlib';
import { resolve, relative } from 'node:path';
import * as v from 'valibot';
const root = resolve(import.meta.dirname, '..');
const files = [
  'index.html',
  'package.json',
  'package-lock.json',
  'tsconfig.json',
  'vite.config.ts',
];
/** @param {string} directory @returns {Promise<string[]>} */
async function walk(directory) {
  const result = [];
  for (const entry of await readdir(resolve(root, directory), { withFileTypes: true })) {
    const path = `${directory}/${entry.name}`;
    if (entry.isDirectory()) result.push(...(await walk(path)));
    else result.push(path);
  }
  return result.sort((a, b) => a.localeCompare(b));
}
/** FNV-1a is a deterministic freshness checksum, not a security signature. */
/** @param {Uint8Array} bytes */
function checksum(bytes) {
  let hash = 0xcbf29ce484222325n;
  for (const byte of bytes) hash = BigInt.asUintN(64, (hash ^ BigInt(byte)) * 0x100000001b3n);
  return hash.toString(16).padStart(16, '0');
}
const inputs = [];
for (const path of [...files, ...(await walk('src')), ...(await walk('scripts'))].sort((a, b) =>
  a.localeCompare(b),
)) {
  inputs.push({ path, checksum: checksum(await readFile(resolve(root, path))) });
}
// Preserve runtime dependency/font licenses in the executable as a local asset.
const rawLock = /** @type {unknown} */ (
  JSON.parse(await readFile(resolve(root, 'package-lock.json'), 'utf8'))
);
const lock = v.parse(
  v.object({
    packages: v.record(
      v.string(),
      v.object({ dev: v.optional(v.boolean()), version: v.optional(v.string()) }),
    ),
  }),
  rawLock,
);
const notices = [];
for (const [path, entry] of Object.entries(lock.packages)) {
  if (!path.startsWith('node_modules/') || entry.dev) continue;
  const directory = resolve(root, path);
  const licenses = (await readdir(directory, { withFileTypes: true })).filter(
    (file) => file.isFile() && /^(license|copying|notice)(\.|$)/i.test(file.name),
  );
  if (licenses.length === 0) throw new Error(`Missing runtime license: ${path}`);
  notices.push(
    `\n${path.slice('node_modules/'.length)} ${entry.version ?? ''}\n${'='.repeat(60)}\n`,
  );
  for (const file of licenses) notices.push(await readFile(resolve(directory, file.name), 'utf8'));
}
const noticeBytes = Buffer.from(notices.join('\n'));
const noticePath = `assets/licenses-${checksum(noticeBytes)}.txt`;
await writeFile(resolve(root, `dist/${noticePath}`), noticeBytes);
const htmlPath = resolve(root, 'dist/index.html');
const html = await readFile(htmlPath, 'utf8');
await writeFile(
  htmlPath,
  html.replace('</head>', `<link rel="license" href="/${noticePath}"></head>`),
);
const assets = [];
let initialBytes = 0;
for (const path of await walk('dist')) {
  if (path.startsWith('dist/.') || path.endsWith('.gz') || path.endsWith('build-seal.json'))
    continue;
  const bytes = await readFile(resolve(root, path));
  const compressed = gzipSync(bytes, { level: 9 });
  await writeFile(resolve(root, `${path}.gz`), compressed);
  const name = relative(resolve(root, 'dist'), resolve(root, path));
  assets.push({ path: name, checksum: checksum(bytes), gzip_checksum: checksum(compressed) });
  if (name.endsWith('.css') || /^assets\/index-[^/]+\.js$/.test(name))
    initialBytes += compressed.length;
}
// Workers, lazy grammars and fonts have independent loading budgets.
if (initialBytes > 120 * 1024)
  throw new Error(`Initial JS/CSS exceeds 120 KiB gzip: ${initialBytes}`);
await writeFile(resolve(root, 'dist/build-seal.json'), JSON.stringify({ inputs, assets }, null, 2));
console.log(`Initial JS/CSS: ${(initialBytes / 1024).toFixed(1)} KiB gzip (budget 120 KiB)`);
