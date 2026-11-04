/** Check the actual embedded script, without maintaining a generated copy. */
'use strict';
const fs = require('node:fs');
const path = require('node:path');
const acorn = require('acorn');
const html = fs.readFileSync(path.join(__dirname, '../../webui.html'), 'utf8');
const scripts = Array.from(html.matchAll(/<script[^>]*>([\s\S]*?)<\/script>/g));
if (scripts.length !== 1) throw new Error('Expected one authoritative inline script');
acorn.parse(scripts[0][1], { ecmaVersion: 2017 });
console.log('Embedded JavaScript parses as ES2017');
