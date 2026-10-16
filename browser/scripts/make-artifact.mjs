import { readFileSync, writeFileSync, mkdirSync } from 'node:fs';

const src = readFileSync(new URL('../dist/index.html', import.meta.url), 'utf8');
const style = src.match(/<style>[\s\S]*?<\/style>/)?.[0] ?? '';
const script = src.match(/<script[^>]*>[\s\S]*?<\/script>/)?.[0] ?? '';
const body = src.match(/<body>([\s\S]*)<\/body>/)?.[1]?.trim() ?? '';

if (!style || !script || !body) {
  console.error('could not extract style/script/body from dist/index.html');
  process.exit(1);
}

const out = `${style}\n${body}\n${script}\n`;
mkdirSync(new URL('../artifact', import.meta.url), { recursive: true });
writeFileSync(new URL('../artifact/sektor7.html', import.meta.url), out);
console.log(`wrote artifact/sektor7.html (${(out.length / 1024).toFixed(1)} kB)`);
