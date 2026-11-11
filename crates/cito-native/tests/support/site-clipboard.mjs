// purpose: execute the shipped copy-button JavaScript against Clipboard API outcomes.
// usage: node site-clipboard.mjs (invoked by the Rust integration suite).
// effects: none outside a VM context.
// requires: Node.js standard library only.
import assert from 'node:assert/strict';
import {readFileSync} from 'node:fs';
import {runInNewContext} from 'node:vm';
const source=readFileSync(new URL('../../../../site/scripts/hero-preview.js',import.meta.url),'utf8');
function fixture(writeText,command='brew install --cask tefx/tap/cito'){const badge={textContent:'Copy',setAttribute(){}};let click;const button={disabled:false,getAttribute:()=>command,querySelector:()=>badge,addEventListener:(_e,f)=>{click=f}};runInNewContext(source,{document:{querySelectorAll:s=>s==='[data-copy-command]'?[button]:[],querySelector:()=>null},navigator:{clipboard:writeText?{writeText}:undefined}});return {badge,button,click:()=>click()};}
let resolve;const writes=[];const f=fixture(command=>{writes.push(command);return new Promise(r=>{resolve=r})});const pending=f.click();assert.equal(f.badge.textContent,'Copy');assert.equal(f.button.disabled,true);await f.click();assert.deepEqual(writes,['brew install --cask tefx/tap/cito']);resolve();await pending;assert.equal(f.badge.textContent,'Copied ✓');assert.equal(f.button.disabled,false);
const fallback=fixture(async command=>assert.equal(command,'brew install --cask tefx/tap/cito'),null);await fallback.click();assert.equal(fallback.badge.textContent,'Copied ✓');
let attempt=0;const retry=fixture(async()=>{if(++attempt===1)throw Error('Permission denied')});await retry.click();assert.equal(retry.badge.textContent,'Copy failed');assert.equal(retry.button.disabled,false);await retry.click();assert.equal(retry.badge.textContent,'Copied ✓');const missing=fixture();await missing.click();assert.equal(missing.badge.textContent,'Copy failed');assert.equal(missing.button.disabled,false);
