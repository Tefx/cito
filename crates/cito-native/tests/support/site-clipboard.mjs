// purpose: execute shipped clipboard and workflow controls using the actual page markup.
// usage: node site-clipboard.mjs (invoked by the Rust integration suite).
// effects: none outside VM contexts.
// requires: Node.js standard library only.
import assert from 'node:assert/strict';
import {existsSync, readFileSync, readdirSync} from 'node:fs';
import {runInNewContext} from 'node:vm';
const site = new URL('../../../../site/', import.meta.url);
const source = readFileSync(new URL('scripts/hero-preview.js', site), 'utf8');

function fixture(writeText, command = 'brew install --cask tefx/tap/cito', code = null) {
  const badge = {textContent: 'Copy', setAttribute() {}};
  let click, now = 0, nextTimer = 0;
  const timers = new Map();
  const button = {
    disabled: false, getAttribute: () => command, querySelector: () => badge,
    hasAttribute: name => name === 'data-copy-code' && code !== null,
    closest: selector => {
      assert.equal(selector, '.code-block');
      return {querySelector: selector => {
        assert.equal(selector, 'pre code');
        return {textContent: code};
      }};
    },
    addEventListener: (_event, handler) => { click = handler; }
  };
  runInNewContext(source, {
    document: {querySelectorAll: selector => selector === '[data-copy-command], [data-copy-code]' ? [button] : [], querySelector: () => null},
    navigator: {clipboard: writeText ? {writeText} : undefined},
    setTimeout: (callback, delay) => { timers.set(++nextTimer, {at: now + delay, callback}); return nextTimer; },
    clearTimeout: timer => timers.delete(timer)
  });
  return {badge, button, click: () => click(), advance(ms) {
    now += ms;
    for (const [id, timer] of timers) if (timer.at <= now) {
      timers.delete(id);
      timer.callback();
    }
  }};
}
let resolve;
const writes = [];
const f = fixture(command => { writes.push(command); return new Promise(r => { resolve = r; }); });
const pending = f.click();
assert.equal(f.badge.textContent, 'Copy');
assert.equal(f.button.disabled, true);
await f.click();
assert.deepEqual(writes, ['brew install --cask tefx/tap/cito']);
resolve();
await pending;
assert.equal(f.badge.textContent, 'Copied ✓');
assert.equal(f.button.disabled, false);
f.advance(1999);
assert.equal(f.badge.textContent, 'Copied ✓');
f.advance(1);
assert.equal(f.badge.textContent, 'Copy');
const fallback = fixture(async command => assert.equal(command, 'brew install --cask tefx/tap/cito'), null);
await fallback.click();
assert.equal(fallback.badge.textContent, 'Copied ✓');
let attempt = 0;
const retry = fixture(async () => { if (++attempt === 1) throw Error('Permission denied'); });
await retry.click();
assert.equal(retry.badge.textContent, 'Copy failed');
assert.equal(retry.button.disabled, false);
await retry.click();
assert.equal(retry.badge.textContent, 'Copied ✓');
const missing = fixture();
await missing.click();
assert.equal(missing.badge.textContent, 'Copy failed');
assert.equal(missing.button.disabled, false);

const pages = ['index.html', ...readdirSync(new URL('docs/', site)).filter(name => name.endsWith('.html')).map(name => 'docs/' + name)];
for (const required of ['index.html', 'docs/guide.html', 'docs/hosting.html', 'docs/plugins.html', 'docs/architecture.html']) {
  assert.ok(pages.includes(required), `Missing required page: ${required}`);
}
const documents = new Map(pages.map(page => [new URL(page, site).href, readFileSync(new URL(page, site), 'utf8')]));
// Compare variable-name cells within each environment table, not mentions elsewhere.
function environmentVars(text, section, html = false) {
  const heading = html ? section.replaceAll('&', '&amp;') : section;
  const table = html
    ? text.match(new RegExp(`<h3>${heading}</h3>\\s*<div\\b[^>]*><table\\b[^>]*>([\\s\\S]*?)</table>`))
    : text.match(new RegExp(`^### ${heading}\\s*\\n([\\s\\S]*?)(?=^### |^## |(?![\\s\\S]))`, 'm'));
  assert.ok(table, `Missing ${section} table`);
  const rows = html
    ? [...table[1].matchAll(/<tr>\s*<td><code>(CITO_[^<]+)<\/code><\/td>/g)]
    : [...table[1].matchAll(/^\|\s*`(CITO_[^\x60]+)`\s*\|/gm)];
  const vars = rows.map(row => row[1].replaceAll('&lt;', '<').replaceAll('&gt;', '>')).sort();
  assert.ok(vars.length > 0, `Empty ${section} table`);
  assert.equal(new Set(vars).size, vars.length, `Duplicate ${section} variable`);
  return vars;
}
const hubTables = [
  ['README.md', readFileSync(new URL('../README.md', site), 'utf8'), false],
  ['deploy/hub/README.md', readFileSync(new URL('../deploy/hub/README.md', site), 'utf8'), false],
  ['site/docs/hosting.html', documents.get(new URL('docs/hosting.html', site).href), true]
];
for (const [section, sample] of [
  ['Hub Server', 'CITO_HUB_PORT'],
  ['Client & Daemon', 'CITO_STATE_DIR'],
  ['Agent & Channel Plugins', 'CITO_PLUGIN_PATH']
]) {
  const tables = hubTables.map(([name, text, html]) => [name, environmentVars(text, section, html)]);
  if (section === 'Hub Server') assert.ok(tables[0][1].includes('CITO_OIDC_REDIRECT_PORT'), 'Hub Server redirect port missing');
  for (const [name, vars] of tables.slice(1)) assert.deepEqual(vars, tables[0][1], `${name}: ${section} variables differ from README.md`);
  for (const [name, text, html] of hubTables) {
    const cell = html ? `<td><code>${sample}</code></td>` : `| \`${sample}\` |`;
    assert.ok(text.includes(cell), `${name}: missing ${section} test row`);
    const missing = text.replace(cell, html ? '<td>removed</td>' : '| removed |');
    assert.notDeepEqual(environmentVars(missing, section, html), tables[0][1], `${name}: missing ${section} row must be detected`);
  }
}

const homepage = documents.get(new URL('index.html', site).href);
const download = 'https://github.com/Tefx/cito/releases/latest/download/Cito-macos-arm64.zip';
for (const [page, html] of documents) {
  const ids = [...html.matchAll(/\bid="([^"]+)"/g)].map(match => match[1]);
  assert.equal(new Set(ids).size, ids.length, `${page}: duplicate IDs`);
  assert.doesNotMatch(html, /class="[^"]*\bdocs-figure-stage--compact\b/, page);
  for (const control of html.matchAll(/<(?:button|input)\b([^>]*)(?:>([\s\S]*?)<\/button>|\s*\/?>)/g)) {
    assert.doesNotMatch(`${control[1]} ${control[2] || ''}`, /Paste Invite Code|My Macs|Desktop Banner|BannerWindow/, page);
  }
  for (const link of html.matchAll(/<(a|link)\b([^>]*)>(?:([\s\S]*?)<\/a>)?/g)) {
    const attrs = Object.fromEntries([...link[2].matchAll(/([\w-]+)="([^"]*)"/g)].map(match => [match[1], match[2]]));
    assert.ok(attrs.href, `${page}: ${link[1]} missing href`);
    const target = new URL(attrs.href.replaceAll('&amp;', '&'), page);
    if (target.protocol === 'file:') {
      const fragment = decodeURIComponent(target.hash.slice(1));
      target.hash = '';
      target.search = '';
      assert.ok(existsSync(target), `${page}: broken link ${attrs.href}`);
      if (fragment) {
        const destination = documents.get(target.href);
        assert.ok(destination, `${page}: fragment points outside site pages: ${attrs.href}`);
        assert.ok([...destination.matchAll(/\bid="([^"]+)"/g)].some(match => match[1] === fragment), `${page}: missing anchor ${attrs.href}`);
      }
    } else {
      assert.ok(['https:', 'http:', 'mailto:'].includes(target.protocol), `${page}: unsupported link ${attrs.href}`);
    }
    if (link[1] === 'a' && attrs.href.startsWith('https://github.com/Tefx/cito')) {
      assert.equal(attrs.target, '_blank', page);
      assert.ok(attrs.rel?.split(/\s+/).includes('noopener') && attrs.rel?.split(/\s+/).includes('noreferrer'), page);
      if (/Download Cito\.app|Cito-macos-arm64\.zip/.test(link[3] || '')) assert.equal(attrs.href, download, page);
    }
  }
}
// Exercise every shipped documentation code block through the shared handler.
// Decode HTML text as the browser does, so copying escaped examples proves the boundary.
function decodedCode(html) {
  return html.replace(/&(#x[0-9a-f]+|#\d+|amp|lt|gt|quot|apos);/gi, (_match, entity) => {
    const named = {amp: '&', lt: '<', gt: '>', quot: '"', apos: "'"};
    if (entity.startsWith('#')) return String.fromCodePoint(entity[1].toLowerCase() === 'x' ? parseInt(entity.slice(2), 16) : Number(entity.slice(1)));
    return named[entity];
  });
}
for (const [page, html] of documents) {
  assert.match(html, /<a class="skip-link" href="#main-content">/);
  if (!page.includes('/docs/')) continue;
  const blocks = [...html.matchAll(/<div class="code-block">\s*<button\b([^>]*)>([\s\S]*?)<\/button>\s*<pre><code(?: [^>]*)?>([\s\S]*?)<\/code><\/pre>\s*<\/div>/g)];
  assert.equal(blocks.length, [...html.matchAll(/<pre>/g)].length, `${page}: every pre needs a copy control`);
  assert.ok(blocks.length > 0, `${page}: no code examples`);
  for (const [, attrs, label, code] of blocks) {
    assert.match(attrs, /\bdata-copy-code\b/);
    assert.match(attrs, /type="button"/);
    assert.match(attrs, /aria-label="Copy code block"/);
    assert.match(label, /<span data-copy-badge>Copy<\/span>/);
    const expected = decodedCode(code);
    const writes = [];
    const block = fixture(async text => writes.push(text), null, expected);
    await block.click();
    assert.deepEqual(writes, [expected], page);
    assert.equal(block.badge.textContent, 'Copied ✓');
    block.advance(1999);
    assert.equal(block.badge.textContent, 'Copied ✓');
    block.advance(1);
    assert.equal(block.badge.textContent, 'Copy');
  }
}
const exact = '<tag key="value"> & text\n  indented\n';
let finishCode;
const codeWrites = [];
const code = fixture(text => { codeWrites.push(text); return new Promise(resolve => { finishCode = resolve; }); }, null, exact);
const copy = code.click();
await code.click();
assert.deepEqual(codeWrites, [exact]);
assert.equal(code.button.disabled, true);
finishCode();
await copy;
code.advance(1500);
const secondCopy = code.click();
code.advance(500);
assert.equal(code.badge.textContent, 'Copied ✓', 'An old reset must not run during a newer pending copy');
finishCode();
await secondCopy;
code.advance(1999);
assert.equal(code.badge.textContent, 'Copied ✓');
code.advance(1);
assert.equal(code.badge.textContent, 'Copy');
const codeRetry = fixture(async () => { throw Error('Permission denied'); }, null, exact);
await codeRetry.click();
assert.equal(codeRetry.badge.textContent, 'Copy failed');
assert.equal(codeRetry.button.disabled, false);
codeRetry.advance(2000);
assert.equal(codeRetry.badge.textContent, 'Copy');

const diagram = readFileSync(new URL('assets/art/architecture-topology.svg', site), 'utf8');
assert.match(diagram, /<title id="title">[^<]+<\/title>/);
assert.match(diagram, /<desc id="desc">[^<]+<\/desc>/);
const expected = [
  ['menu-dark.png', 'directory-dark.png'],
  ['menu-dark.png', 'activity-dark.png'],
  ['settings-apps-dark.png', 'settings-channels-dark.png', 'settings-hubs-dark.png']
];
const ids = [...homepage.matchAll(/\bid="([^"]+)"/g)].map(match => match[1]);
assert.equal(new Set(ids).size, ids.length, 'Page IDs must be unique across workflow previews');
let focused;
function element(markup) {
  const attrs = Object.fromEntries([...markup.matchAll(/([\w-]+)="([^"]*)"/g)].map(match => [match[1], match[2]]));
  const handlers = {};
  return {
    getAttribute: name => attrs[name],
    setAttribute: (name, value) => { attrs[name] = value; },
    removeAttribute: name => { delete attrs[name]; },
    hidden: /\bhidden\b/.test(markup),
    addEventListener: (event, handler) => { handlers[event] = handler; },
    dispatch: (event, value) => handlers[event](value),
    focus() { focused = this; }
  };
}
function proportionalImage(markup) {
  const image = element(markup.match(/<img\b([^>]+)>/)[1]);
  assert.equal(image.getAttribute('class'), 'screenshot-img');
  const pixels = readFileSync(new URL(image.getAttribute('src').replace(/^\.\.\//, '').replace(/^\.\//, ''), site));
  const width = pixels.readUInt32BE(16);
  const height = pixels.readUInt32BE(20);
  assert.equal(Number(image.getAttribute('width')), width);
  assert.equal(Number(image.getAttribute('height')), height);
  assert.equal(image.getAttribute('style'), `--logical-w: ${width / 2}; --logical-h: ${height / 2};`);
}
const moments = [...homepage.matchAll(/<article class="moment-card">([\s\S]*?)<\/article>/g)];
assert.equal(moments.length, 3);
const previews = moments.map((moment, index) => {
  const html = moment[1];
  assert.equal([...html.matchAll(/data-surface-preview/g)].length, 1);
  const tabs = [...html.matchAll(/<button\b([^>]*data-preview-tab[^>]*)>/g)].map(match => element(match[1]));
  const stages = [...html.matchAll(/<div class="moment-stage-panel"([^>]*)>([\s\S]*?)\n            <\/div>/g)];
  assert.deepEqual(stages.map(match => match[2].match(/screenshots\/([^"]+)/)[1]), expected[index]);
  const panels = stages.map(match => {
    assert.match(match[2], /<div class="screenshot-stage-viewport">/);
    assert.match(match[2], /<div class="screenshot-callout">\s*\S[\s\S]*<\/div>/);
    proportionalImage(match[2]);
    return element(match[1]);
  });
  assert.equal(tabs.length, panels.length);
  for (const [i, tab] of tabs.entries()) {
    assert.equal(tab.getAttribute('aria-controls'), panels[i].getAttribute('id'));
    assert.equal(panels[i].getAttribute('aria-labelledby'), tab.getAttribute('id'));
    assert.equal(tab.getAttribute('data-preview-tab'), panels[i].getAttribute('data-preview-panel'));
    assert.equal(tab.getAttribute('aria-selected'), String(i === 0));
    assert.equal(panels[i].hidden, i !== 0);
  }
  return {tabs, panels, querySelectorAll: selector => selector === '[data-preview-tab]' ? tabs : panels};
});
assert.equal([...homepage.matchAll(/src="\.\/assets\/screenshots\//g)].length, 7);
runInNewContext(source, {
  document: {
    querySelectorAll: selector => ({
      '[data-surface-preview]': previews
    }[selector] || []),
    querySelector: () => null
  }, navigator: {}
});
function active(preview, index) {
  for (const [i, tab] of preview.tabs.entries()) {
    assert.equal(tab.getAttribute('aria-selected'), String(i === index));
    assert.equal(tab.tabIndex, i === index ? 0 : -1);
    assert.equal(preview.panels[i].hidden, i !== index);
  }
}
const selected = [0, 0, 0];
for (const [index, preview] of previews.entries()) {
  active(preview, 0);
  for (const [i, tab] of preview.tabs.entries()) {
    tab.dispatch('click');
    selected[index] = i;
    for (const [other, group] of previews.entries()) active(group, selected[other]);
  }
  for (const [key, target] of [['Home', 0], ['ArrowLeft', preview.tabs.length - 1], ['ArrowRight', 0], ['End', preview.tabs.length - 1]]) {
    let prevented = false;
    preview.tabs[selected[index]].dispatch('keydown', {key, preventDefault() { prevented = true; }});
    selected[index] = target;
    assert.equal(prevented, true);
    assert.equal(focused, preview.tabs[target]);
    for (const [other, group] of previews.entries()) active(group, selected[other]);
  }
  preview.tabs[selected[index]].dispatch('keydown', {key: 'Tab', preventDefault() { assert.fail('Tab must keep native focus navigation'); }});
  active(preview, selected[index]);
}
const guide = readFileSync(new URL('docs/guide.html', site), 'utf8');
const headings = [...guide.matchAll(/<h2 id="([^"]+)">/g)];
const sections = Object.fromEntries(headings.map((heading, index) => [
  heading[1], guide.slice(heading.index, headings[index + 1]?.index ?? guide.length)
]));
for (const [section, screenshots] of Object.entries({
  'settings-walkthrough': ['menu-dark.png', 'directory-dark.png', 'activity-dark.png'],
  'agent-apps': ['settings-apps-dark.png'],
  'telegram-pairing': ['settings-channels-dark.png'],
  'lan-and-team-hubs': ['settings-hubs-dark.png']
})) {
  const figures = [...sections[section].matchAll(/<figure\b[^>]*>([\s\S]*?)<\/figure>/g)];
  assert.deepEqual(figures.map(match => match[1].match(/screenshots\/([^"]+)/)[1]), screenshots);
  for (const figure of figures) {
    assert.match(figure[1], /<div class="docs-figure-stage">/);
    assert.match(figure[1], /<figcaption\b[^>]*>\s*\S[\s\S]*<\/figcaption>/);
    proportionalImage(figure[1]);
  }
  assert.equal([...sections[section].matchAll(/class="docs-figure docs-figure--staged"/g)].length, screenshots.length);
}
const daily = sections['settings-walkthrough'];
assert.equal([...daily.matchAll(/<h3>/g)].length, 3);
for (const part of daily.split('<h3>').slice(1)) assert.equal([...part.matchAll(/<figure\b/g)].length, 1);
const sharing = sections['lan-and-team-hubs'];
assert.ok(sharing.indexOf('settings-hubs-dark.png') < sharing.indexOf('<h3>Joining a Shared Team Hub'));
