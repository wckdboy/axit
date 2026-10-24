import assert from 'node:assert/strict';
import {readFile,stat} from 'node:fs/promises';
import {dirname,join} from 'node:path';
import {fileURLToPath} from 'node:url';
const root=dirname(fileURLToPath(import.meta.url));const dist=join(root,'dist');
const html=await readFile(join(dist,'index.html'),'utf8');
assert(html.includes('<title>Axit'));assert(html.includes('https://axit.sh/'));assert(html.includes('https://github.com/wckdboy/axit'));
const ids=new Set([...html.matchAll(/\bid="([^"]+)"/g)].map(m=>m[1]));
for(const match of html.matchAll(/\b(?:href|src)="([^"]+)"/g)){
  const url=match[1];
  if(url.startsWith('#'))assert(ids.has(url.slice(1)),`Missing anchor ${url}`);
  else if(url.startsWith('/')&&url!=='/')assert((await stat(join(dist,url))).isFile(),`Missing asset ${url}`);
}
assert.equal(await readFile(join(dist,'install.sh'),'utf8'),await readFile(join(root,'../install.sh'),'utf8'));
assert((await readFile(join(dist,'terminal.svg'),'utf8')).includes('AXIT'));
assert((await readFile(join(dist,'_headers'),'utf8')).includes('Cache-Control: no-store'));
assert((await readFile(join(dist,'fonts/OFL.txt'),'utf8')).includes('SIL OPEN FONT LICENSE'));
console.log('Site checks passed: links, anchors, brand, installer identity, preview and font license.');
