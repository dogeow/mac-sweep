const test = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const css = fs.readFileSync(path.join(__dirname, '../frontend/style.css'), 'utf8');
const html = fs.readFileSync(path.join(__dirname, '../frontend/index.html'), 'utf8');
const light = css.slice(0, css.indexOf('* {'));
const color = (name) => light.match(new RegExp(String.raw`--${name}:\s*(#[0-9a-f]{3,6})`, 'i'))[1];
function luminance(hex) {
  if (hex.length === 4) hex = '#' + [...hex.slice(1)].map(value => value + value).join('');
  const rgb = hex.slice(1).match(/../g).map(value => parseInt(value, 16) / 255).map(value => value <= .04045 ? value / 12.92 : ((value + .055) / 1.055) ** 2.4);
  return rgb[0] * .2126 + rgb[1] * .7152 + rgb[2] * .0722;
}
test('light low-space warnings remain readable on the actual card and toolbar surfaces', () => {
  for (const foreground of [color('disk-low'), color('disk-warning')]) for (const background of [color('bg'), color('toolbar'), color('sidebar')]) {
    const a = luminance(foreground), b = luminance(background);
    assert.ok((Math.max(a, b) + .05) / (Math.min(a, b) + .05) >= 4.5, `${foreground} on ${background}`);
  }
});
test('detailed tables reserve width for every column and continuous progress is not a live region', () => {
  assert.match(css, /simple-mode:not\(\.show-details\).*th:nth-child\(1\)/);
  assert.match(css, /show-details \.analysis-table \{ min-width: 610px/);
  const progress = html.match(/<div[^>]*id="analysis-progress"[^>]*>/)[0];
  assert.match(progress, /aria-live="off"/);
  assert.equal(progress.includes('role="status"'), false);
  assert.match(html, /id="analysis-progress-announcement" role="status" aria-live="polite"/);
  assert.match(html, /id="analysis-breadcrumb-previous"/);
  assert.match(html, /id="analysis-breadcrumb-next"/);
});
