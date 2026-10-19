const fs = require('node:fs');
const vm = require('node:vm');
const assert = require('node:assert/strict');
const html = fs.readFileSync('src/index.html', 'utf8');
const match = html.match(/<script>([\s\S]*?)<\/script>/);
assert.ok(match, 'embedded UI script exists');
new vm.Script(match[1], { filename: 'web-editor.js' });
for (const id of ['visualBlocks','draftPreview','useH1','useH2','useBullet','useQuote','useBold','useItalic']) {
  assert.ok(html.includes('id="' + id + '"'), 'expected toolbar or preview element: ' + id);
}
class Element {
  constructor(tag) { this.tagName = tag.toUpperCase(); this.children=[];this.value='';this._text=''; }
  append(...nodes) { this.children.push(...nodes); }
  get lastElementChild() { return this.children.at(-1) || null; }
  get textContent() { return this._text; }
  set textContent(value) { this._text=String(value);this.children=[]; }
}
const source = match[1].slice(match[1].indexOf('function renderRichBlock('),match[1].indexOf('function renderDraft('));
const context={document:{createElement:(tag)=>new Element(tag)}};
vm.runInNewContext(source,context);
const root = new Element('div');
const dangerous='<img src=x onerror=alert(1)>';
context.renderRichBlock(root,'heading2',dangerous,{bold:true});
assert.equal(root.children[0].tagName,'H2');
assert.equal(root.children[0].children[0].tagName,'STRONG');
assert.equal(root.children[0].children[0].textContent,dangerous);
context.renderRichBlock(root,'bullet','one');
context.renderRichBlock(root,'bullet','two');
assert.equal(root.children[1].tagName,'UL');
assert.equal(root.children[1].children.length,2);
context.renderRichBlock(root,'ordered','three');
assert.equal(root.children[2].tagName,'OL');
context.renderRichBlock(root,'unknown','ignored');
assert.equal(root.children.length,3);
console.log('Web editor JS syntax, semantic rendering and safe text-node smoke passed');
