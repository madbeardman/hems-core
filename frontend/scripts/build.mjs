import { readFile, mkdir, writeFile } from 'node:fs/promises';
import { fileURLToPath } from 'node:url';
import { createHash } from 'node:crypto';
const root = new URL('../', import.meta.url);
const read = path => readFile(new URL(path, root), 'utf8');
const model = await read('src/model.mjs');
const app = (await read('src/app.mjs')).replace(/^import .* from '\.\/model\.mjs';\n/m, '');
const js = `/* Built by frontend/scripts/build.mjs; edit frontend/src instead. */\n${model}\n${app}`;
const css = await read('src/styles.css');
const hash = createHash('sha256').update(js + css).digest('hex').slice(0, 12);
const html = (await read('src/index.html')).replaceAll('__BUILD__', hash);
await mkdir(new URL('dist/', root), { recursive: true });
for (const [name, content] of [['index.html', html], ['app.js', js], ['styles.css', css]]) {
  await writeFile(new URL(`dist/${name}`, root), content);
  process.stdout.write(`${name}: ${Buffer.byteLength(content)} bytes\n`);
}
process.stdout.write(`Built ${hash} in ${fileURLToPath(new URL('dist/', root))}\n`);
