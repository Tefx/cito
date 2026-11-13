// purpose: execute shipped clipboard and workflow controls using the actual page markup.
// usage: node site-clipboard.mjs (invoked by the Rust integration suite).
// effects: none outside VM contexts.
// requires: Node.js standard library only.
import assert from 'node:assert/strict';
import {readFileSync} from 'node:fs';
import {runInNewContext} from 'node:vm';
const site = new URL('../../../../site/', import.meta.url);
const source = readFileSync(new URL('scripts/hero-preview.js', site), 'utf8');

function fixture(writeText, command = 'brew install --cask tefx/tap/cito') {
  const badge = {textContent: 'Copy', setAttribute() {}};
  let click;
  const button = {
    disabled: false, getAttribute: () => command, querySelector: () => badge,
    addEventListener: (_event, handler) => { click = handler; }
  };
  runInNewContext(source, {
    document: {querySelectorAll: selector => selector === '[data-copy-command]' ? [button] : [], querySelector: () => null},
    navigator: {clipboard: writeText ? {writeText} : undefined}
  });
  return {badge, button, click: () => click()};
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

const homepage = readFileSync(new URL('index.html', site), 'utf8');
const download = 'https://github.com/Tefx/cito/releases/latest/download/Cito-macos-arm64.zip';
for (const page of ['index.html', 'docs/guide.html', 'docs/plugins.html', 'docs/architecture.html']) {
  const html = readFileSync(new URL(page, site), 'utf8');
  for (const anchor of html.matchAll(/<a\b([^>]*)>([\s\S]*?)<\/a>/g)) {
    const attrs = Object.fromEntries([...anchor[1].matchAll(/([\w-]+)="([^"]*)"/g)].map(match => [match[1], match[2]]));
    if (!attrs.href?.startsWith('https://github.com/Tefx/cito')) continue;
    assert.equal(attrs.target, '_blank', page);
    assert.ok(attrs.rel.split(' ').includes('noopener') && attrs.rel.split(' ').includes('noreferrer'), page);
    if (/Download Cito\.app|Cito-macos-arm64\.zip/.test(anchor[2])) assert.equal(attrs.href, download, page);
  }
}
const diagram = readFileSync(new URL('assets/art/architecture-topology.svg', site), 'utf8');
assert.match(diagram, /<title id="title">[^<]+<\/title>/);
assert.match(diagram, /<desc id="desc">[^<]+<\/desc>/);
const expected = [
  ['menu-dark.png', 'directory-dark.png'],
  ['banner-dark.png', 'activity-dark.png'],
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
  'telegram-pairing': ['settings-channels-dark.png', 'banner-dark.png'],
  'lan-and-team-hubs': ['settings-hubs-dark.png']
})) {
  const figures = [...sections[section].matchAll(/<figure\b[^>]*>([\s\S]*?)<\/figure>/g)];
  assert.deepEqual(figures.map(match => match[1].match(/screenshots\/([^"]+)/)[1]), screenshots);
  for (const figure of figures) {
    assert.match(figure[1], /<div class="docs-figure-stage(?: docs-figure-stage--compact)?">/);
    assert.match(figure[1], /<figcaption\b[^>]*>\s*\S[\s\S]*<\/figcaption>/);
    proportionalImage(figure[1]);
  }
  assert.equal([...sections[section].matchAll(/class="docs-figure docs-figure--staged"/g)].length, screenshots.length);
}
assert.match(sections['telegram-pairing'], /docs-figure-stage--compact[\s\S]*banner-dark\.png/);
const daily = sections['settings-walkthrough'];
assert.equal([...daily.matchAll(/<h3>/g)].length, 3);
for (const part of daily.split('<h3>').slice(1)) assert.equal([...part.matchAll(/<figure\b/g)].length, 1);
const sharing = sections['lan-and-team-hubs'];
assert.ok(sharing.indexOf('settings-hubs-dark.png') < sharing.indexOf('<h3>Joining a Shared Team Hub'));
