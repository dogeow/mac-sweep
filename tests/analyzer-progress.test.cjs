const test = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const vm = require('node:vm');
const navigation = require('../frontend/analysis-navigation.js');
const timing = require('../frontend/analysis-timing.js');

// Drive the real analyzer with deferred native replies and a controlled clock.
// This checks async lifetime and truthful progress, without relying on a disk scan.
async function harness(options = {}) {
  let now = 0;
  let busy = false;
  let timerId = 0;
  let progressListener;
  let view = options.view || 'analysis';
  const favoriteList = (options.favorites || []).map((favorite) => ({ ...favorite }));
  const favoriteCalls = [];
  const favoriteNotices = [];
  const timers = new Map();
  const windowListeners = new Map();
  const pending = [];
  const calls = [];
  const inspections = [];
  const reveals = [];
  const registry = new Map();
  const byPath = new Map();
  const browsedPaths = new Set();
  let browseSequence = 0;
  const clone = (value) => JSON.parse(JSON.stringify(value));
  function register(value, command) {
    if (value?.cancelled) return value;
    if (value?.analysisId && value.root) {
      if (command === 'analyze_directory') browsedPaths.clear();
      registry.set(value.analysisId, clone(value));
      const stack = [value.root];
      while (stack.length) { const current = stack.pop(); if (current === value.root || command === 'analyze_directory' || !byPath.has(current.path) || byPath.get(current.path).id !== current.id) byPath.set(current.path, clone(current)); stack.push(...(current.children || []).slice().reverse()); }
      if (command === 'browse_analysis_directory') browsedPaths.add(value.root.path);
    }
    return value;
  }
  function knownReply(request) {
    const source = registry.get(request.args.analysisId);
    const stack = source ? [source.root] : [];
    let target;
    while (stack.length) { const current = stack.pop(); if (current.id === request.args.nodeId) { target = current; break; } stack.push(...(current.children || [])); }
    if (!target) return null;
    const current = byPath.get(target.path) || target;
    if (current.hasChildren && !current.children.length && !browsedPaths.has(current.path)) return null;
    return { ...clone(source), analysisId: `live-browse-${++browseSequence}`, sourceAnalysisId: source.sourceAnalysisId || source.analysisId, cachedBrowse: true, scanComplete: false, root: { ...clone(current), id: target.id, sizeSource: current.sizeKnown === false ? 'unknown' : 'cached' }, durationMs: 1 };
  }
  const document = { activeElement: null, addEventListener() {} };
  let tableScroll;
  class Element {
    constructor(id, classes = '') {
      this.id = id;
      this.children = new Map();
      this.listeners = new Map();
      this.classes = new Set(classes.split(/\s+/).filter(Boolean));
      this.classList = {
        add: (name) => this.classes.add(name),
        remove: (name) => this.classes.delete(name),
        contains: (name) => this.classes.has(name),
        toggle: (name, enabled) => {
          enabled = enabled === undefined ? !this.classes.has(name) : enabled;
          if (enabled) this.classes.add(name); else this.classes.delete(name);
          return enabled;
        },
      };
      this.textContent = '';
      this.writes = 0;
      this.style = {};
      this.scrollTop = 0;
    }
    set className(value) { this.classes = new Set(value.split(/\s+/).filter(Boolean)); }
    get className() { return [...this.classes].join(' '); }
    set innerHTML(value) {
      this.html = value;
      this.writes += 1;
      this.children.clear();
      for (const match of value.matchAll(/class="([^"]+)"/g)) {
        for (const name of match[1].split(/\s+/)) this.children.set(`.${name}`, new Element(name));
      }
      if (value.includes('data-analysis-cancel')) this.children.set('[data-analysis-cancel]', new Element('cancel'));
    }
    get innerHTML() { return this.html || ''; }
    querySelector(selector) {
      if (selector === 'summary' && !this.children.has(selector)) this.children.set(selector, new Element('summary'));
      return this.children.get(selector) || null;
    }
    querySelectorAll() { return []; }
    closest(selector) { return this.id === 'analysis-rows' && selector === '.analysis-table-scroll' ? tableScroll : null; }
    setAttribute(name, value) { this[name] = String(value); }
    removeAttribute(name) { delete this[name]; }
    addEventListener(name, listener) { this.listeners.set(name, listener); }
    focus() { document.activeElement = this; }
  }
  const html = fs.readFileSync(path.join(__dirname, '../frontend/index.html'), 'utf8');
  const elements = new Map();
  for (const match of html.matchAll(/<[^>]*\bid="([^"]+)"[^>]*>/g)) {
    elements.set(match[1], new Element(match[1], match[0].match(/class="([^"]+)"/)?.[1] || ''));
  }
  tableScroll = new Element('analysis-table-scroll', 'analysis-table-scroll');
  document.getElementById = (id) => elements.get(id) || null;
  document.querySelector = (selector) => selector === '.analysis-table-scroll' ? tableScroll : null;
  const app = {
    desktop: true, demo: false, getView: () => view, isBusy: () => busy,
    showAnalysis: () => { if (busy) return false; view = 'analysis'; windowListeners.get('mac-sweep-view')?.(); return true; },
    showHome: () => { if (busy) return false; view = 'home'; windowListeners.get('mac-sweep-view')?.(); return true; },
    setAnalysisBusy: (value) => { busy = value; },
    icon: () => '', escapeHtml: (value) => String(value).replaceAll('&', '&amp;').replaceAll('<', '&lt;').replaceAll('"', '&quot;'),
  };
  const window = {
    macSweep: app, macSweepAnalysisNavigation: navigation, macSweepAnalysisTiming: timing,
    setInterval: (callback, delay) => { assert.equal(delay, 1000); const id = ++timerId; timers.set(id, callback); return id; },
    clearInterval: (id) => timers.delete(id),
    addEventListener: (name, listener) => windowListeners.set(name, listener),
    __TAURI__: {
      core: { invoke: (command, args) => {
        calls.push({ command, args });
        if (command === 'get_analysis_locations') return Promise.resolve([{ id: 'home', label: '我的文件', path: '/example' }]);
        if (command === 'cancel_analysis') return Promise.resolve();
        if (command === 'inspect_analysis_node' || command === 'reveal_analysis_node') {
          const queue = command === 'inspect_analysis_node' ? inspections : reveals;
          const result = queue.length ? queue.shift() : { status: command === 'inspect_analysis_node' ? 'ready' : 'shown' };
          if (result instanceof Error) throw result;
          return Promise.resolve(result);
        }
        if (command === 'browse_analysis_directory' && inspections.length) {
          const result = inspections.shift();
          if (result instanceof Error) throw result;
          return Promise.resolve(result).then((value) => {
            if (value?.status === 'changed' || value?.status === 'missing') throw `ANALYSIS_NODE_${value.status.toUpperCase()}: fixture`;
            if (value?.analysisId && value.root) return register(value, command);
            const reply = knownReply({ args });
            if (!reply) throw new Error('Fixture directory has no known listing');
            return register(reply, command);
          });
        }
        if (command === 'analyze_directory' || command === 'browse_analysis_directory' || command === 'resolve_favorite_directory' || command === 'open_favorite_directory') return new Promise((resolve, reject) => pending.push({ command, args, resolve: (value) => resolve(register(value, command)), reject }));
        throw new Error(`Unexpected native command: ${command}`);
      } },
      event: { listen: (_, listener) => { progressListener = listener; return Promise.resolve(() => {}); } },
    },
  };
  if (options.favorites) window.macSweepFavorites = {
    has: (path) => favoriteList.some((favorite) => favorite.path === path),
    get: (id) => favoriteList.find((favorite) => favorite.id === id) || null,
    isBusy: () => false, canEdit: () => !options.readOnlyFavorites,
    icon: (filled) => filled ? '★' : '☆', setCurrent: () => {}, note: (text) => favoriteNotices.push(text),
    toggle: async (args) => { favoriteCalls.push(args); },
  };
  const source = fs.readFileSync(path.join(__dirname, '../frontend/analyzer.js'), 'utf8');
  vm.runInNewContext(source, { window, document, performance: { now: () => now } });
  await new Promise(setImmediate);
  async function settleKnown(expectedNodeId = null, expectedPath = null) {
    await new Promise(setImmediate);
    if (pending[0]?.command === 'browse_analysis_directory') {
      const reply = expectedNodeId && pending[0].args.nodeId !== expectedNodeId ? null : knownReply(pending[0]);
      if (reply && expectedPath && reply.root.path !== expectedPath) return;
      if (reply) { pending.shift().resolve(reply); await new Promise(setImmediate); }
    }
  }
  const originalFavorite = window.macSweepAnalyzer.openFavorite;
  window.macSweepAnalyzer.openFavorite = async (id) => { const request = originalFavorite(id); await settleKnown(null, favoriteList.find(item => item.id === id)?.path); return request; };
  return {
    elements, tableScroll, timers, pending, calls, favoriteCalls, favoriteNotices, app: window.macSweepAnalyzer, getView: () => view,
    emit: (payload) => progressListener({ payload }),
    tick: (milliseconds) => { now += milliseconds; for (const callback of [...timers.values()]) callback(); },
    unload: () => windowListeners.get('beforeunload')(),
    clickNode: async (id) => { const directory = window.macSweepAnalyzer.contextTarget(id)?.kind === 'directory'; elements.get('analysis-view').listeners.get('click')({ target: { closest: (selector) => selector === '[data-analysis-node]' ? { dataset: { analysisNode: id } } : null } }); if (directory) await settleKnown(id); else await new Promise(setImmediate); },
    back: async () => { elements.get('analysis-back').listeners.get('click')(); await settleKnown(); },
    breadcrumb: async (index) => { elements.get('analysis-breadcrumbs').listeners.get('click')({ target: { closest: () => ({ dataset: { historyIndex: String(index) } }) } }); await settleKnown(); },
    selectLocation: (value) => elements.get('analysis-location').listeners.get('change')({ target: { value } }),
    sort: (value) => elements.get(value === 'name-asc' ? 'analysis-sort-name' : 'analysis-sort-size').listeners.get('click')(),
    nextPage: () => elements.get('analysis-table-footer').listeners.get('click')({ target: { closest: () => ({ dataset: { analysisPage: 'next' } }) } }),
    settle: () => new Promise(setImmediate),
    queueInspect: (...results) => inspections.push(...results),
    queueBrowse: (...results) => inspections.push(...results),
    queueReveal: (...results) => reveals.push(...results),
    revealCurrent: async () => { elements.get('analysis-reveal').listeners.get('click')(); await new Promise(setImmediate); },
  };
}
const node = (overrides = {}) => ({ sizeSource: 'scan', id: 'root', path: '/example', name: 'example', kind: 'directory', bytes: 8192, files: 1, children: [], hasChildren: false, partial: false, omittedChildren: 0, ...overrides });
const report = (overrides = {}) => ({ analysisId: 'fixture', sourceAnalysisId: overrides.analysisId || 'fixture', scanComplete: true, root: node(), scannedFiles: 1, warnings: [], durationMs: 1000, ...overrides });
const rowIds = (h) => [...h.elements.get('analysis-rows').innerHTML.matchAll(/data-analysis-row="([^"]+)"/g)].map((match) => match[1]);
function assertSelectedLocation(h, expected) {
  const options = [...h.elements.get('analysis-location').innerHTML.matchAll(/<option\b([^>]*)>/g)];
  const selected = options.filter((match) => /(?:^|\s)selected(?:\s|=|$)/.test(match[1])).map((match) => match[1].match(/\bvalue="([^"]*)"/)[1]);
  assert.deepEqual(selected, [expected]);
  assert.equal(h.elements.get('analysis-selected-path').textContent, expected || '选择文件夹以查看实际占用');
  assert.equal(h.elements.get('analysis-selected-path').title, expected);
}

test('sorting buttons immediately reorder the current list and select exactly one mode', async () => {
  const h = await harness();
  const children = [
    node({ id: 'small', name: 'Alpha', path: '/example/Alpha', kind: 'file', bytes: 100 }),
    node({ id: 'large', name: 'Zulu', path: '/example/Zulu', kind: 'file', bytes: 300 }),
  ];
  const scan = h.app.scan();
  h.pending.shift().resolve(report({ root: node({ children, hasChildren: true }) }));
  await scan;
  assert.deepEqual(rowIds(h), ['large', 'small']);
  assert.equal(h.elements.get('analysis-sort-size')['aria-pressed'], 'true');
  assert.equal(h.elements.get('analysis-sort-name')['aria-pressed'], 'false');
  const requests = h.calls.length;
  h.sort('name-asc');
  assert.deepEqual(rowIds(h), ['small', 'large']);
  assert.equal(h.elements.get('analysis-sort-size')['aria-pressed'], 'false');
  assert.equal(h.elements.get('analysis-sort-name')['aria-pressed'], 'true');
  h.sort('size-desc');
  assert.deepEqual(rowIds(h), ['large', 'small']);
  assert.equal(h.elements.get('analysis-sort-size')['aria-pressed'], 'true');
  assert.equal(h.elements.get('analysis-sort-name')['aria-pressed'], 'false');
  assert.equal(h.calls.length, requests);
});

test('a sorting preference chosen before analysis applies to the first result', async () => {
  const h = await harness();
  assert.equal(h.elements.get('analysis-display-options').classList.contains('hidden'), true);
  assert.equal(h.elements.get('analysis-options-hint').classList.contains('hidden'), false);
  assert.equal(Boolean(h.elements.get('analysis-sort-size').disabled), false);
  assert.equal(Boolean(h.elements.get('analysis-sort-name').disabled), false);
  h.sort('name-asc');
  assert.equal(h.elements.get('analysis-sort-name')['aria-pressed'], 'true');
  assert.deepEqual(rowIds(h), []);
  const scan = h.app.scan();
  h.pending.shift().resolve(report({ root: node({ children: [
    node({ id: 'large', name: 'Zulu', path: '/example/Zulu', bytes: 300 }),
    node({ id: 'small', name: 'Alpha', path: '/example/Alpha', bytes: 100 }),
  ], hasChildren: true }) }));
  await scan;
  assert.deepEqual(rowIds(h), ['small', 'large']);
  assert.equal(h.elements.get('analysis-display-options').classList.contains('hidden'), false);
  assert.equal(h.elements.get('analysis-options-hint').classList.contains('hidden'), true);
});

test('an existing list can be sorted while a replacement analysis is pending', async () => {
  const h = await harness();
  const root = node({ children: [
    node({ id: 'small', name: 'Alpha', path: '/example/Alpha', bytes: 100 }),
    node({ id: 'large', name: 'Zulu', path: '/example/Zulu', bytes: 300 }),
  ], hasChildren: true });
  const scan = h.app.scan();
  h.pending.shift().resolve(report({ root }));
  await scan;
  const rescan = h.app.scan();
  assert.equal(h.elements.get('start-scan').disabled, true);
  assert.equal(Boolean(h.elements.get('analysis-sort-name').disabled), false);
  const requests = h.calls.length;
  h.sort('name-asc');
  assert.deepEqual(rowIds(h), ['small', 'large']);
  assert.equal(h.calls.length, requests);
  assert.equal(h.pending.length, 1);
  h.pending.shift().resolve(report({ analysisId: 'replacement', root }));
  await rescan;
  assert.deepEqual(rowIds(h), ['small', 'large']);
  assert.equal(h.elements.get('analysis-sort-name')['aria-pressed'], 'true');
});

test('changing sort resets the current page, saved ancestor pages, and table scroll', async () => {
  const h = await harness();
  const makeFiles = (prefix, directory) => Array.from({ length: 101 }, (_, index) => node({
    id: `${prefix}-${index}`, name: `File ${String(index).padStart(3, '0')}`,
    path: `${directory}/file-${index}`, kind: 'file', bytes: 200 - index,
  }));
  const folder = node({ id: 'folder', name: 'Folder', path: '/example/folder', bytes: 0, children: makeFiles('child', '/example/folder'), hasChildren: true });
  const scan = h.app.scan();
  h.pending.shift().resolve(report({ root: node({ children: [...makeFiles('parent', '/example'), folder], hasChildren: true }) }));
  await scan;
  h.nextPage();
  assert.match(h.elements.get('analysis-table-footer').innerHTML, /2 \/ 2/);
  assert.equal(rowIds(h).includes('folder'), true);
  await h.clickNode('folder');
  h.nextPage();
  assert.match(h.elements.get('analysis-table-footer').innerHTML, /2 \/ 2/);
  assert.deepEqual(rowIds(h), ['child-100']);
  h.tableScroll.scrollTop = 900;
  h.sort('name-asc');
  assert.match(h.elements.get('analysis-table-footer').innerHTML, /1 \/ 2/);
  assert.equal(rowIds(h)[0], 'child-0');
  assert.equal(rowIds(h).length, 100);
  assert.equal(h.tableScroll.scrollTop, 0);
  await h.back();
  assertSelectedLocation(h, '/example');
  assert.match(h.elements.get('analysis-table-footer').innerHTML, /1 \/ 2/);
  assert.equal(rowIds(h)[0], 'parent-0');
  assert.equal(rowIds(h).length, 100);
});

test('size sorting keeps known empty items ahead of unknown sizes and breaks ties by displayed name', async () => {
  const h = await harness();
  const scan = h.app.scan();
  h.pending.shift().resolve(report({ root: node({ children: [
    node({ id: 'applications', name: 'Applications', path: '/example/Applications', bytes: 40 }),
    node({ id: 'unknown-high', name: 'Unknown high', path: '/example/unknown-high', bytes: 9999, sizeKnown: false }),
    node({ id: 'empty', name: 'Empty', path: '/example/empty', bytes: 0 }),
    node({ id: 'documents', name: 'Documents', path: '/example/Documents', bytes: 40 }),
    node({ id: 'unknown-low', name: 'Unknown low', path: '/example/unknown-low', bytes: 0, sizeKnown: false }),
    node({ id: 'big', name: 'Big', path: '/example/big', bytes: 50 }),
  ], hasChildren: true }) }));
  await scan;
  assert.deepEqual(rowIds(h), ['big', 'documents', 'applications', 'empty', 'unknown-high', 'unknown-low']);
  assert.match(h.elements.get('analysis-rows').innerHTML, /文稿/);
  assert.match(h.elements.get('analysis-rows').innerHTML, /应用程序/);
  h.sort('name-asc');
  const ids = rowIds(h);
  assert.equal(ids.indexOf('documents') < ids.indexOf('applications'), true);
});

test('event-based estimated progress stays truthful while elapsed time advances without filenames or cancel focus loss', async () => {
  const h = await harness();
  const scan = h.app.scan();
  const empty = h.elements.get('analysis-empty');
  assert.equal(h.timers.size, 1);
  assert.match(empty.querySelector('.analysis-empty-title').textContent, /正在完整分析我的文件/);
  assert.match(empty.querySelector('.analysis-empty-progress').textContent, /0 个文件/);
  const meter = empty.querySelector('.analysis-empty-meter');
  const label = empty.querySelector('.analysis-meter-label');
  const track = empty.querySelector('.analysis-meter-track');
  const fill = empty.querySelector('.analysis-meter-fill');
  assert.equal(meter.classList.contains('is-indeterminate'), true);
  assert.equal(label.textContent, '准备中');
  assert.equal(Object.hasOwn(track, 'aria-valuenow'), false);
  const cancelButton = empty.querySelector('[data-analysis-cancel]');
  cancelButton.focus();
  const writes = empty.writes;
  h.emit({ scannedFiles: 1234, bytesFound: 3000, currentPath: '/example/Documents/readme.txt', estimatedPercent: 37.5 });
  assert.match(empty.querySelector('.analysis-empty-progress').textContent, /1,234 个文件.*3 KB/);
  assert.equal(empty.querySelector('.analysis-empty-location'), null);
  assert.equal(h.elements.get('analysis-progress-path').textContent, '');
  assert.equal(h.elements.get('analysis-progress-path').title, '');
  assert.equal(label.textContent, '预计 37.5%');
  assert.equal(track['aria-valuenow'], '37.5');
  assert.equal(track['aria-valuemin'], '0');
  assert.equal(track['aria-valuemax'], '100');
  assert.equal(fill.style.width, '37.5%');
  assert.equal(meter.classList.contains('is-indeterminate'), false);
  h.elements.get('analysis-details-toggle').listeners.get('click')();
  assert.equal(empty.querySelector('.analysis-empty-location'), null);
  assert.equal(h.elements.get('analysis-progress-path').title, '');
  const realValues = empty.querySelector('.analysis-empty-progress').textContent;
  h.emit({ currentPath: '/example/cache', scannedFiles: NaN, bytesFound: -1 });
  assert.equal(empty.querySelector('.analysis-empty-progress').textContent, realValues);
  assert.equal(label.textContent, '预计 37.5%');
  assert.equal(fill.style.width, '37.5%');
  h.tick(5000);
  assert.match(empty.querySelector('.analysis-empty-time').textContent, /已用 5 秒.*等待系统返回/);
  assert.equal(empty.querySelector('.analysis-empty-progress').textContent, realValues);
  assert.equal(empty.writes, writes);
  assert.strictEqual(empty.querySelector('[data-analysis-cancel]'), cancelButton);
  h.pending.shift().resolve(report());
  await scan;
  assert.equal(h.timers.size, 0);
  h.emit({ scannedFiles: 9999, bytesFound: 9999 });
  assert.match(h.elements.get('analysis-progress-count').textContent, /1,234/);
});

test('unknown estimates remain indeterminate and invalid event percentages cannot announce completion', async () => {
  const h = await harness();
  const scan = h.app.scan();
  const empty = h.elements.get('analysis-empty');
  const wrapper = empty.querySelector('.analysis-empty-meter');
  const label = empty.querySelector('.analysis-meter-label');
  const track = empty.querySelector('.analysis-meter-track');
  h.emit({ estimatedPercent: null, scannedFiles: 10, bytesFound: 3000, currentPath: '/example/secret.txt' });
  h.tick(12000);
  assert.equal(label.textContent, '正在估算');
  assert.equal(wrapper.classList.contains('is-indeterminate'), true);
  assert.equal(Object.hasOwn(track, 'aria-valuenow'), false);
  h.emit({ estimatedPercent: 42, scannedFiles: 20, bytesFound: 5000 });
  for (const value of [100, 120, -1, NaN, '90']) h.emit({ estimatedPercent: value, scannedFiles: 21 });
  assert.equal(label.textContent, '预计 42%');
  assert.equal(track['aria-valuenow'], '42');
  h.pending.shift().resolve({ analysisId: 'malformed', root: {} });
  await scan;
  assert.equal(label.textContent, '预计 42%');
  h.emit({ estimatedPercent: 99, scannedFiles: 1000 });
  assert.equal(track['aria-valuenow'], '42');
});

test('real analyzer ETA counts down from observed work, waits honestly, and resets after cancellation', async () => {
  const h = await harness();
  const scan = h.app.scan();
  const empty = h.elements.get('analysis-empty');
  const eta = h.elements.get('analysis-progress-eta');
  const track = h.elements.get('analysis-progress-track');
  const fill = h.elements.get('analysis-progress-fill');
  assert.equal(eta.textContent, '正在估算剩余时间…');

  h.tick(10_000);
  h.emit({ estimatedPercent: 10, scannedFiles: 10, bytesFound: 4096 });
  assert.equal(eta.textContent, '预计剩余 01:30');
  assert.equal(empty.querySelector('.analysis-empty-eta').textContent, eta.textContent);
  h.tick(10_000);
  h.emit({ estimatedPercent: 20, scannedFiles: 20, bytesFound: 8192 });
  assert.equal(eta.textContent, '正在更新预计时间…');
  h.tick(1_000);
  h.emit({ estimatedPercent: 21, scannedFiles: 21, bytesFound: 8500 });
  assert.equal(eta.textContent, '正在更新预计时间…');
  h.tick(1_000);
  h.emit({ estimatedPercent: 22, scannedFiles: 22, bytesFound: 9000 });
  assert.match(eta.textContent, /^预计剩余 /);
  h.tick(1_000);
  assert.match(eta.textContent, /^预计剩余 /);
  assert.equal(track['aria-valuenow'], '22');
  assert.equal(fill.style.width, '22%');
  assert.match(h.elements.get('analysis-progress-count').textContent, /22 个文件/);

  h.tick(2_000);
  assert.match(eta.textContent, /^预计剩余 /);
  assert.equal(track['aria-valuenow'], '22');
  assert.equal(fill.style.width, '22%');
  h.emit({ estimatedPercent: 23, scannedFiles: 23, bytesFound: 9500 });
  assert.match(eta.textContent, /^预计剩余 /);
  assert.notEqual(eta.textContent, '预计剩余 00:00');
  assert.equal(track['aria-valuenow'], '23');

  h.emit({ estimatedPercent: null, scannedFiles: 30, bytesFound: 10_000 });
  assert.match(eta.textContent, /^预计剩余 /);
  assert.equal(empty.querySelector('.analysis-meter-label').textContent, '正在估算');
  assert.equal(Object.hasOwn(track, 'aria-valuenow'), false);
  h.emit({ estimatedPercent: 99, scannedFiles: 99, bytesFound: 20_000 });
  assert.equal(eta.textContent, '正在核对剩余内容…');
  assert.equal(track['aria-valuenow'], '99');
  await h.app.cancel();
  assert.equal(eta.textContent, '');
  assert.equal(empty.querySelector('.analysis-empty-eta').textContent, '');
  h.tick(1_000);
  assert.equal(eta.textContent, '');
  assert.equal(track['aria-valuenow'], '99');
  h.pending.shift().resolve(report({ cancelled: true, root: node({ partial: true }) }));
  await scan;
  assert.equal(h.timers.size, 0);
  assert.equal(track['aria-valuenow'], '99');
  assert.equal(h.elements.get('analysis-progress').classList.contains('hidden'), true);

  const rescan = h.app.scan();
  assert.equal(h.timers.size, 1);
  assert.equal(eta.textContent, '正在估算剩余时间…');
  assert.equal(Object.hasOwn(track, 'aria-valuenow'), false);
  h.tick(5_000);
  h.emit({ estimatedPercent: 15, scannedFiles: 15, bytesFound: 4096 });
  assert.equal(eta.textContent, '预计剩余 00:29');
  h.pending.shift().resolve(report());
  await rescan;
  assert.equal(eta.textContent, '本轮分析已完成');
  assert.equal(track['aria-valuenow'], '100');
  assert.equal(fill.style.width, '100%');
  assert.equal(h.timers.size, 0);
  h.tick(120_000);
  h.emit({ estimatedPercent: 99, scannedFiles: 9999 });
  assert.equal(track['aria-valuenow'], '100');
  assert.equal(eta.textContent, '本轮分析已完成');
});

test('transient unknown readings keep one ETA while an exceeded file-count reference immediately invalidates it', async () => {
  const h = await harness();
  const scan = h.app.scan();
  const eta = h.elements.get('analysis-progress-eta');
  const track = h.elements.get('analysis-progress-track');
  h.tick(10_000);
  h.emit({ estimatedPercent: 10, scannedFiles: 10, bytesFound: 4096 });
  assert.match(eta.textContent, /^预计剩余 /);
  for (let cycle = 0; cycle < 6; cycle += 1) {
    h.tick(500);
    h.emit({ estimatedPercent: null, scannedFiles: 20 + cycle * 2, bytesFound: 8192 + cycle });
    assert.match(eta.textContent, /^预计剩余 /);
    assert.equal(Object.hasOwn(track, 'aria-valuenow'), false);
    h.tick(500);
    h.emit({ estimatedPercent: 11 + cycle, scannedFiles: 21 + cycle * 2, bytesFound: 8193 + cycle });
    assert.match(eta.textContent, /^预计剩余 /);
    assert.equal(track['aria-valuenow'], String(11 + cycle));
  }
  h.pending.shift().resolve(report({ analysisId: 'stable-reference', sourceAnalysisId: 'stable-reference', scannedFiles: 2000, root: node({ files: 2000 }) }));
  await scan;

  const rescan = h.app.scan();
  h.tick(5_000);
  h.emit({ estimatedPercent: 30, scannedFiles: 1000, bytesFound: 80_000_000_000 });
  assert.match(eta.textContent, /^预计剩余 /);
  assert.equal(track['aria-valuenow'], '50');
  // Invalidation is not an ordinary brief unknown stream: no grace period.
  h.emit({ estimatedPercent: 30, scannedFiles: 2001, bytesFound: 120_000_000_000 });
  assert.equal(eta.textContent, '正在更新预计时间…');
  assert.equal(Object.hasOwn(track, 'aria-valuenow'), false);
  h.tick(1_000);
  assert.equal(eta.textContent, '正在更新预计时间…');
  await h.app.cancel();
  h.pending.shift().resolve(report({ cancelled: true, scannedFiles: 2001, root: node({ files: 2001, partial: true }) }));
  await rescan;
  assert.equal(h.timers.size, 0);
});

test('short system gaps keep the ETA and a long unknown period needs two stable seconds to recover', async () => {
  const h = await harness();
  const scan = h.app.scan();
  const eta = h.elements.get('analysis-progress-eta');
  const track = h.elements.get('analysis-progress-track');
  h.tick(10_000);
  h.emit({ estimatedPercent: 10, scannedFiles: 10, bytesFound: 4096 });
  assert.equal(eta.textContent, '预计剩余 01:30');
  h.tick(3_000);
  assert.equal(eta.textContent, '预计剩余 01:27');
  assert.equal(track['aria-valuenow'], '10');
  h.emit({ estimatedPercent: null, scannedFiles: 20, bytesFound: 8192 });
  assert.match(eta.textContent, /^预计剩余 /);
  for (let second = 1; second <= 11; second += 1) {
    h.tick(1_000);
    h.emit({ estimatedPercent: null, scannedFiles: 20 + second, bytesFound: 8192 + second });
  }
  assert.equal(eta.textContent, '正在更新预计时间…');
  h.emit({ estimatedPercent: 30, scannedFiles: 100, bytesFound: 10_000 });
  assert.equal(eta.textContent, '正在更新预计时间…');
  h.tick(1_000);
  h.emit({ estimatedPercent: null, scannedFiles: 120, bytesFound: 11_000 });
  assert.equal(eta.textContent, '正在更新预计时间…');
  h.tick(1_000);
  h.emit({ estimatedPercent: 31, scannedFiles: 150, bytesFound: 12_000 });
  assert.equal(eta.textContent, '正在更新预计时间…');
  h.tick(1_000);
  h.emit({ estimatedPercent: 32, scannedFiles: 160, bytesFound: 13_000 });
  assert.equal(eta.textContent, '正在更新预计时间…');
  h.tick(1_000);
  h.emit({ estimatedPercent: 33, scannedFiles: 170, bytesFound: 14_000 });
  assert.match(eta.textContent, /^预计剩余 /);
  h.tick(500);
  h.emit({ estimatedPercent: null, scannedFiles: 180, bytesFound: 15_000 });
  assert.match(eta.textContent, /^预计剩余 /);
  h.tick(500);
  h.emit({ estimatedPercent: 34, scannedFiles: 190, bytesFound: 16_000 });
  assert.match(eta.textContent, /^预计剩余 /);
  h.pending.shift().resolve(report({ scannedFiles: 200, root: node({ files: 200 }) }));
  await scan;
  assert.equal(h.timers.size, 0);
});

test('a same-path rescan advances by its completed file-count reference while backend percent stays coarse', async () => {
  const h = await harness();
  const track = h.elements.get('analysis-progress-track');
  const fill = h.elements.get('analysis-progress-fill');
  const label = h.elements.get('analysis-progress-percent');
  const eta = h.elements.get('analysis-progress-eta');
  const first = h.app.scan();
  h.emit({ estimatedPercent: 30, scannedFiles: 1000, bytesFound: 80_000_000_000 });
  assert.equal(track['aria-valuenow'], '30');
  // Native full reports identify themselves as their own source analysis.
  h.pending.shift().resolve(report({ analysisId: 'first-full', sourceAnalysisId: 'first-full', scannedFiles: 2000, root: node({ files: 2000 }) }));
  await first;

  const rescan = h.app.scan();
  h.tick(10_000);
  h.emit({ estimatedPercent: 30, scannedFiles: 1000, bytesFound: 80_000_000_000 });
  assert.equal(label.textContent, '预计 50%');
  assert.equal(track['aria-valuenow'], '50');
  assert.equal(fill.style.width, '50%');
  assert.equal(eta.textContent, '预计剩余 00:10');
  h.tick(5_000);
  h.emit({ estimatedPercent: 30, scannedFiles: 1200, bytesFound: 96_000_000_000 });
  assert.equal(track['aria-valuenow'], '60');
  h.tick(5_000);
  h.emit({ estimatedPercent: 30, scannedFiles: 1500, bytesFound: 120_000_000_000 });
  assert.equal(label.textContent, '预计 75%');
  assert.equal(track['aria-valuenow'], '75');
  assert.equal(fill.style.width, '75%');
  assert.match(h.elements.get('analysis-progress-count').textContent, /1,500 个文件.*120 GB/);
  assert.match(eta.textContent, /^预计剩余 /);
  h.tick(1_000);
  assert.match(eta.textContent, /^预计剩余 /);
  assert.equal(track['aria-valuenow'], '75');
  // More bytes alone cannot increase the count-based percentage.
  h.emit({ estimatedPercent: 30, scannedFiles: 1500, bytesFound: 160_000_000_000 });
  assert.equal(track['aria-valuenow'], '75');
  h.pending.shift().resolve(report({ analysisId: 'second-full', sourceAnalysisId: 'second-full', scannedFiles: 2000, root: node({ files: 2000 }) }));
  await rescan;
  assert.equal(h.timers.size, 0);
});

test('larger scans become unknown without replacing references on cancellation or failure, and references stay path scoped', async () => {
  const h = await harness();
  const track = h.elements.get('analysis-progress-track');
  const label = h.elements.get('analysis-progress-percent');
  const meter = h.elements.get('analysis-progress-meter');
  const finished = (id, files, overrides = {}) => report({
    analysisId: id, sourceAnalysisId: id, scannedFiles: files,
    root: node({ files }), ...overrides,
  });
  const first = h.app.scan();
  h.pending.shift().resolve(finished('reference-2000', 2000));
  await first;

  const cancelled = h.app.scan();
  h.emit({ estimatedPercent: 30, scannedFiles: 2001, bytesFound: 120_000_000_000 });
  assert.equal(label.textContent, '正在估算');
  assert.equal(Object.hasOwn(track, 'aria-valuenow'), false);
  assert.equal(meter.classList.contains('is-indeterminate'), true);
  h.emit({ estimatedPercent: 30, scannedFiles: 5000, bytesFound: 160_000_000_000 });
  assert.equal(label.textContent, '正在估算');
  await h.app.cancel();
  h.pending.shift().resolve(finished('cancelled-5000', 5000, { cancelled: true, root: node({ partial: true, files: 5000 }) }));
  await cancelled;

  const failed = h.app.scan();
  h.emit({ estimatedPercent: 30, scannedFiles: 1000, bytesFound: 80_000_000_000 });
  assert.equal(track['aria-valuenow'], '30');
  h.pending.shift().reject('fixture reader failed');
  await failed;
  const grown = h.app.scan();
  h.emit({ estimatedPercent: 30, scannedFiles: 1000, bytesFound: 80_000_000_000 });
  assert.equal(track['aria-valuenow'], '30');
  h.pending.shift().resolve(finished('reference-2500', 2500));
  await grown;

  const updated = h.app.scan();
  h.emit({ estimatedPercent: 30, scannedFiles: 1000, bytesFound: 80_000_000_000 });
  assert.equal(label.textContent, '预计 40%');
  assert.equal(track['aria-valuenow'], '40');
  h.pending.shift().resolve(finished('reference-2500-again', 2500));
  await updated;

  h.selectLocation('/different');
  const different = h.app.scan();
  assert.equal(h.pending[0].args.path, '/different');
  h.emit({ estimatedPercent: 30, scannedFiles: 1000, bytesFound: 80_000_000_000 });
  assert.equal(label.textContent, '预计 30%');
  assert.equal(track['aria-valuenow'], '30');
  h.pending.shift().resolve(finished('different-reference', 1000, { root: node({ path: '/different', name: 'different', files: 1000 }) }));
  await different;
  assert.equal(h.timers.size, 0);
});

test('only a valid non-cancelled report reaches 100, while cancelled partial reports retain their estimate', async () => {
  for (const cancelled of [false, true]) {
    const h = await harness();
    const scan = h.app.scan();
    const empty = h.elements.get('analysis-empty');
    const label = empty.querySelector('.analysis-meter-label');
    const track = empty.querySelector('.analysis-meter-track');
    h.emit({ estimatedPercent: 65, scannedFiles: 12, bytesFound: 9000 });
    if (cancelled) await h.app.cancel();
    h.pending.shift().resolve(report({ cancelled, root: node({ partial: true }), warnings: ['部分目录无权读取'] }));
    await scan;
    assert.equal(track['aria-valuenow'], cancelled ? '65' : '100');
    assert.equal(label.textContent, cancelled ? '预计 65%' : '已完成 100%');
    assert.equal(h.elements.get('analysis-warnings').classList.contains('hidden'), false);
    h.emit({ estimatedPercent: 99 });
    assert.equal(track['aria-valuenow'], cancelled ? '65' : '100');
    assert.equal(h.timers.size, 0);
  }
});

test('rescanning resets the finished meter, consumes real estimates, and hides it during directory browsing', async () => {
  const h = await harness();
  const child = node({ id: 'child', path: '/example/child', name: 'child' });
  const scan = h.app.scan();
  h.pending.shift().resolve(report({ root: node({ children: [child], hasChildren: true }) }));
  await scan;
  const wrapper = h.elements.get('analysis-progress-meter');
  const label = h.elements.get('analysis-progress-percent');
  const track = h.elements.get('analysis-progress-track');
  const fill = h.elements.get('analysis-progress-fill');
  assert.equal(label.textContent, '已完成 100%');
  const rescan = h.app.scan();
  assert.equal(wrapper.classList.contains('hidden'), false);
  assert.equal(wrapper.classList.contains('is-indeterminate'), true);
  assert.equal(label.textContent, '准备中');
  assert.equal(Object.hasOwn(track, 'aria-valuenow'), false);
  h.emit({ estimatedPercent: 0, scannedFiles: 0, bytesFound: 0 });
  assert.equal(label.textContent, '预计 0%');
  assert.equal(fill.style.width, '0%');
  h.emit({ estimatedPercent: 64.3, scannedFiles: 10, bytesFound: 8192 });
  h.tick(4000);
  assert.equal(track['aria-valuenow'], '64.3');
  assert.equal(fill.style.width, '64.3%');
  h.pending.shift().resolve(report({ root: node({ children: [child], hasChildren: true }) }));
  await rescan;
  let reply;
  h.queueInspect(new Promise((resolve) => { reply = resolve; }));
  await h.clickNode('child');
  assert.equal(wrapper.classList.contains('hidden'), true);
  assert.equal(label.textContent, '');
  assert.equal(Object.hasOwn(track, 'aria-valuenow'), false);
  reply({ status: 'ready' });
  await h.settle();
});

test('refresh keeps prior data, resets counters, and clears timers on rejected or malformed replies', async () => {
  const h = await harness();
  const first = h.app.scan();
  const file = node({ id: 'file', path: '/example/notes.txt', name: 'notes.txt', kind: 'file', bytes: 4096 });
  h.pending.shift().resolve(report({ root: node({ children: [file], hasChildren: true }) }));
  await first;
  const rows = h.elements.get('analysis-rows').innerHTML;
  for (const malformed of [false, true]) {
    const refresh = h.app.scan();
    assert.equal(h.timers.size, 1);
    assert.match(h.elements.get('analysis-rows').innerHTML, /notes\.txt/);
    assert.match(h.elements.get('analysis-progress-label').textContent, /上次结果.*已用 0 秒/);
    assert.match(h.elements.get('analysis-progress-count').textContent, /0 个文件.*0 B/);
    h.tick(1000);
    assert.match(h.elements.get('analysis-progress-label').textContent, /已用 1 秒/);
    const reply = h.pending.shift();
    if (malformed) reply.resolve({ analysisId: 'bad', root: {} }); else reply.reject(new Error('Disk unavailable'));
    await refresh;
    assert.equal(h.timers.size, 0);
    assert.equal(h.elements.get('analysis-rows').innerHTML, rows);
    assert.match(h.elements.get('analysis-message').textContent, /已保留上次结果/);
  }
});

test('cancellation waits for the partial result, then ends its ticker; unloading also clears it', async () => {
  const h = await harness();
  const scan = h.app.scan();
  h.emit({ scannedFiles: 22, bytesFound: 2048, currentPath: '/example/cache' });
  await h.app.cancel();
  assert.equal(h.timers.size, 1);
  assert.match(h.elements.get('analysis-empty').querySelector('.analysis-empty-title').textContent, /正在停止/);
  h.tick(2000);
  assert.match(h.elements.get('analysis-empty').querySelector('.analysis-empty-time').textContent, /已用 2 秒.*等待系统停止/);
  h.pending.shift().resolve(report({ cancelled: true, root: node({ partial: true }) }));
  await scan;
  assert.equal(h.timers.size, 0);
  const next = h.app.scan();
  assert.equal(h.timers.size, 1);
  h.unload();
  assert.equal(h.timers.size, 0);
  h.pending.shift().resolve(report());
  await next;
  assert.equal(h.timers.size, 0);
});

test('unexpanded directories browse by verified IDs, retain parent statistics, and reuse completed results', async () => {
  const h = await harness();
  const library = node({ id: 'library', path: '/example/Library', name: 'Library', bytes: 2000000, files: 12345, hasChildren: true, omittedChildren: 2 });
  const scan = h.app.scan();
  h.pending.shift().resolve(report({ root: node({ bytes: 3000000, children: [library], hasChildren: true }) }));
  await scan;
  assertSelectedLocation(h, '/example');
  const parentRows = h.elements.get('analysis-rows').innerHTML;
  const parentStatus = h.elements.get('analysis-status').textContent;
  await h.clickNode('library');
  assertSelectedLocation(h, '/example');
  assert.equal(h.timers.size, 0);
  assert.match(h.elements.get('analysis-progress-label').textContent, /正在打开目录/);
  assert.equal(h.elements.get('analysis-progress-count').textContent, '');
  assert.equal(h.elements.get('analysis-progress-count').classList.contains('hidden'), true);
  const browse = h.pending.shift();
  assert.equal(browse.command, 'browse_analysis_directory');
  assert.equal(browse.args.analysisId, 'fixture');
  assert.equal(browse.args.nodeId, 'library');
  assert.equal(Object.hasOwn(browse.args, 'path'), false);
  const unmeasured = node({ id: 'cache', path: '/example/Library/Caches', name: 'Caches', bytes: 0, files: 0, sizeKnown: false, sizeSource: 'unknown', partial: true, hasChildren: true });
  browse.resolve(report({ analysisId: 'browse-fixture', cachedBrowse: true, sourceAnalysisId: 'fixture', measuredAt: 1791000000000, root: { ...library, children: [unmeasured], omittedChildren: 0 }, durationMs: 20, scannedFiles: 9999999 }));
  await h.settle();
  assertSelectedLocation(h, '/example/Library');
  assert.match(h.elements.get('analysis-rows').innerHTML, /Caches.*待分析/);
  assert.equal(h.elements.get('analysis-rows').innerHTML.includes('0 B'), false);
  assert.match(h.elements.get('analysis-status').textContent, /2 MB.*已有统计/);
  assert.match(h.elements.get('analysis-snapshot').textContent, /12,345 个文件.*已有统计.*统计于.*打开耗时/);
  assert.equal(h.elements.get('analysis-snapshot').textContent.includes('9,999,999'), false);
  assert.equal(h.elements.get('scan-button-text').textContent, '完整分析');
  await h.back();
  assertSelectedLocation(h, '/example');
  assert.deepEqual(rowIds(h), [...parentRows.matchAll(/data-analysis-row="([^"]+)"/g)].map(match => match[1]));
  assert.match(h.elements.get('analysis-status').textContent, /已有统计/);
  const requests = h.calls.length;
  await h.clickNode('library');
  assertSelectedLocation(h, '/example/Library');
  assert.equal(h.calls.length, requests + 1);
  assert.equal(h.calls.at(-1).command, 'browse_analysis_directory');
  assert.match(h.elements.get('analysis-status').textContent, /已有统计/);
  h.elements.get('analysis-chart-toggle').listeners.get('click')();
  assert.equal(h.elements.get('analysis-chart').innerHTML.includes('data-analysis-node="cache"'), false);
  assert.match(h.elements.get('analysis-chart-caption').textContent, /完整分析/);
  await h.clickNode('cache');
  assert.equal(typeof h.pending[0].args.analysisId, 'string');
  assert.equal(h.pending[0].args.nodeId, 'cache');
  h.pending.shift().resolve(report({ analysisId: 'deeper-browse', cachedBrowse: true, scanComplete: false, root: { ...unmeasured, children: [], hasChildren: false } }));
  await h.settle();
  assertSelectedLocation(h, '/example/Library/Caches');
  assert.match(h.elements.get('analysis-status').textContent, /待分析/);
  assert.equal(h.elements.get('analysis-warnings').classList.contains('hidden'), true);
  assert.match(h.elements.get('analysis-empty').innerHTML, /目录大小尚未统计/);
  await h.breadcrumb(0);
  assertSelectedLocation(h, '/example');
  assert.deepEqual(rowIds(h), [...parentRows.matchAll(/data-analysis-row="([^"]+)"/g)].map(match => match[1]));
  await h.clickNode('library');
  const rescan = h.app.scan();
  const request = h.pending.shift();
  assert.equal(request.command, 'analyze_directory');
  assert.equal(request.args.path, '/example/Library');
  request.resolve(report({ analysisId: 'library-full', root: { ...library, id: 'library-full-root' } }));
  await rescan;
  assertSelectedLocation(h, '/example/Library');
  assert.match(h.elements.get('analysis-breadcrumbs').innerHTML, /data-history-index="1"/);
  await h.back();
  assertSelectedLocation(h, '/example');
  assert.deepEqual(rowIds(h), [...parentRows.matchAll(/data-analysis-row="([^"]+)"/g)].map(match => match[1]));
});

test('loaded directory navigation selects the current folder and full analysis keeps its ancestors', async () => {
  const h = await harness();
  const support = node({ id: 'support', path: '/example/Library/Application Support', name: 'Application Support' });
  const library = node({ id: 'library', path: '/example/Library', name: 'Library', children: [support], hasChildren: true });
  const scan = h.app.scan();
  h.pending.shift().resolve(report({ root: node({ children: [library], hasChildren: true }) }));
  await scan;
  const requests = h.calls.length;
  await h.clickNode('library');
  assertSelectedLocation(h, '/example/Library');
  assert.match(h.elements.get('analysis-rows').innerHTML, /Application Support/);
  assert.equal(h.elements.get('scan-button-text').textContent, '完整分析');
  await h.clickNode('support');
  assertSelectedLocation(h, '/example/Library/Application Support');
  assert.equal(h.elements.get('analysis-current-name').textContent, 'Application Support');
  await h.back();
  assertSelectedLocation(h, '/example/Library');
  assert.match(h.elements.get('analysis-rows').innerHTML, /Application Support/);
  await h.clickNode('support');
  await h.breadcrumb(0);
  assertSelectedLocation(h, '/example');
  assert.match(h.elements.get('analysis-rows').innerHTML, /应用与系统文件/);
  assert.equal(h.calls.slice(requests).every((call) => call.command === 'browse_analysis_directory'), true);
  await h.clickNode('support');
  assertSelectedLocation(h, '/example/Library/Application Support');
  assert.match(h.elements.get('analysis-breadcrumbs').innerHTML, /data-history-index="2"/);
  const rescan = h.app.scan();
  const request = h.pending.shift();
  assert.equal(request.command, 'analyze_directory');
  assert.equal(request.args.path, '/example/Library/Application Support');
  request.resolve(report({ analysisId: 'support-full', root: { ...support, id: 'support-full-root' } }));
  await rescan;
  assertSelectedLocation(h, '/example/Library/Application Support');
  assert.match(h.elements.get('analysis-breadcrumbs').innerHTML, /data-history-index="2"/);
  await h.back();
  assertSelectedLocation(h, '/example/Library');
  await h.back();
  assertSelectedLocation(h, '/example');
});

test('an explicitly selected location controls the next full scan without an automatic scan', async () => {
  const h = await harness();
  const library = node({ id: 'library', path: '/example/Library', name: 'Library' });
  const scan = h.app.scan();
  h.pending.shift().resolve(report({ root: node({ children: [library], hasChildren: true }) }));
  await scan;
  await h.clickNode('library');
  const requests = h.calls.length;
  h.selectLocation('/example/Documents');
  assertSelectedLocation(h, '/example/Documents');
  assert.equal(h.calls.length, requests);
  assert.equal(h.elements.get('scan-button-text').textContent, '开始分析');
  assert.match(h.elements.get('analysis-message').textContent, /已选择文稿/);
  const selectedScan = h.app.scan();
  const request = h.pending.shift();
  assert.equal(request.command, 'analyze_directory');
  assert.equal(request.args.path, '/example/Documents');
  request.resolve(report({ root: node({ id: 'documents', path: '/example/Documents', name: 'Documents' }) }));
  await selectedScan;
  assertSelectedLocation(h, '/example/Documents');
  assert.equal(h.elements.get('scan-button-text').textContent, '重新分析');
  assert.equal(h.elements.get('analysis-breadcrumbs').innerHTML.includes('data-history-index="1"'), false);
});

test('failed or cancelled directory opening keeps its parent and a fresh scan invalidates browse cache', async () => {
  const h = await harness();
  const library = node({ id: 'library', path: '/example/Library', name: 'Library', hasChildren: true });
  const original = report({ root: node({ children: [library], hasChildren: true }) });
  const scan = h.app.scan();
  h.pending.shift().resolve(original);
  await scan;
  const parentRows = h.elements.get('analysis-rows').innerHTML;
  await h.clickNode('library');
  h.pending.shift().reject(new Error('Folder unavailable'));
  await h.settle();
  assert.deepEqual(rowIds(h), [...parentRows.matchAll(/data-analysis-row="([^"]+)"/g)].map(match => match[1]));
  assert.match(h.elements.get('analysis-message').textContent, /已保留当前结果/);
  await h.clickNode('library');
  const cancelled = h.pending.shift();
  await h.app.cancel();
  cancelled.resolve(report({ cancelled: true, root: library }));
  await h.settle();
  assert.deepEqual(rowIds(h), [...parentRows.matchAll(/data-analysis-row="([^"]+)"/g)].map(match => match[1]));
  assert.equal(h.timers.size, 0);
  await h.clickNode('library');
  h.pending.shift().resolve(report({ analysisId: 'browse-old', root: { ...library, hasChildren: false } }));
  await h.settle();
  await h.back();
  const fresh = h.app.scan();
  assert.equal(h.pending[0].command, 'analyze_directory');
  h.pending.shift().resolve({ ...original, analysisId: 'fresh-source' });
  await fresh;
  await h.clickNode('library');
  assert.equal(h.pending[0].command, 'browse_analysis_directory');
  assert.equal(h.pending[0].args.analysisId, 'fresh-source');
  h.pending.shift().resolve(report({ analysisId: 'browse-fresh', root: library }));
  await h.settle();
});

test('a missing file is removed, its directory is shallowly refreshed, and old browse caches are invalidated', async () => {
  const h = await harness();
  const library = node({ id: 'library', path: '/example/Library', name: 'Library', bytes: 2000000, hasChildren: true });
  const removed = node({ id: 'removed-file', path: '/example/Library/removed.txt', name: 'removed.txt', kind: 'file', bytes: 900000 });
  const scan = h.app.scan();
  h.pending.shift().resolve(report({ root: node({ bytes: 3000000, children: [library], hasChildren: true }) }));
  await scan;
  await h.clickNode('library');
  h.pending.shift().resolve(report({ analysisId: 'library-old', sourceAnalysisId: 'fixture', cachedBrowse: true, root: { ...library, children: [removed] } }));
  await h.settle();
  await h.back();
  await h.clickNode('library');
  assert.equal(h.pending.length, 0);
  h.queueReveal({ status: 'missing' });
  await h.clickNode('removed-file');
  assert.equal(h.elements.get('analysis-rows').innerHTML.includes('removed.txt'), false);
  const refresh = h.pending.shift();
  assert.equal(refresh.command, 'browse_analysis_directory');
  assert.equal(typeof refresh.args.analysisId, 'string');
  assert.equal(refresh.args.nodeId, 'library');
  refresh.resolve(report({ analysisId: 'library-refreshed', sourceAnalysisId: 'fixture', cachedBrowse: true, root: { ...library, children: [], hasChildren: false } }));
  await h.settle();
  assertSelectedLocation(h, '/example/Library');
  assert.match(h.elements.get('analysis-status').textContent, /2 MB.*占用需更新/);
  assert.match(h.elements.get('analysis-message').textContent, /已被移动或删除，列表已更新/);
  assert.equal(h.elements.get('analysis-message').classList.contains('error'), false);
  assert.equal(h.elements.get('analysis-message').textContent.includes('/example'), false);
  assert.equal(h.elements.get('scan-button-text').textContent, '重新分析');
  await h.back();
  assert.match(h.elements.get('analysis-status').textContent, /3 MB.*占用需更新/);
  await h.clickNode('library');
  assert.equal(h.calls.at(-1).command, 'browse_analysis_directory');
  assert.equal(typeof h.calls.at(-1).args.analysisId, 'string');
  assert.equal(h.pending.length, 0);
  assert.equal(h.calls.filter((call) => call.command === 'analyze_directory').length, 1);
});

test('a missing current folder returns to its registered ancestor and refreshes that list', async () => {
  const h = await harness();
  const child = node({ id: 'inside', path: '/example/folder/inside.txt', name: 'inside.txt', kind: 'file' });
  const folder = node({ id: 'folder', path: '/example/folder', name: 'folder', children: [child], hasChildren: true });
  const scan = h.app.scan();
  h.pending.shift().resolve(report({ root: node({ children: [folder], hasChildren: true }) }));
  await scan;
  await h.clickNode('folder');
  h.queueReveal({ status: 'missing' });
  await h.revealCurrent();
  assert.equal(h.pending[0].args.nodeId, 'root');
  h.pending.shift().resolve(report({ analysisId: 'ancestor-refreshed', sourceAnalysisId: 'fixture', cachedBrowse: true, root: node() }));
  await h.settle();
  assertSelectedLocation(h, '/example');
  assert.equal(h.elements.get('analysis-rows').innerHTML.includes('inside.txt'), false);
  assert.equal(h.elements.get('analysis-breadcrumbs').innerHTML.includes('data-history-index="1"'), false);
  assert.match(h.elements.get('analysis-message').textContent, /已返回上级.*重新分析/);
  assert.equal(h.timers.size, 0);
});

test('cached directory inspection rejects stale folders and fallback filtering allows a new same-name inode', async () => {
  const h = await harness();
  const child = node({ id: 'old-child', path: '/example/folder/old.txt', name: 'old.txt', kind: 'file' });
  const folder = node({ id: 'old-folder', path: '/example/folder', name: 'folder', children: [child], hasChildren: true });
  const scan = h.app.scan();
  h.pending.shift().resolve(report({ root: node({ children: [folder], hasChildren: true }) }));
  await scan;
  h.queueInspect({ status: 'changed' });
  await h.clickNode('old-folder');
  assertSelectedLocation(h, '/example');
  assert.equal(h.elements.get('analysis-rows').innerHTML.includes('old-folder'), false);
  const replacement = node({ id: 'new-folder', path: '/example/folder', name: 'folder', bytes: 0, sizeKnown: false, sizeSource: 'unknown', partial: true, hasChildren: true });
  h.pending.shift().resolve(report({ analysisId: 'fallback-refreshed', sourceAnalysisId: 'fixture', cachedBrowse: true, root: node({ children: [{ ...folder, children: [] }, replacement], hasChildren: true, partial: true }), blockedDirectoryCount: 1, otherErrorCount: 1 }));
  await h.settle();
  const rows = h.elements.get('analysis-rows').innerHTML;
  assert.equal(rows.includes('data-analysis-node="old-folder"'), false);
  assert.equal(rows.includes('data-analysis-node="new-folder"'), true);
  assert.match(rows, /待分析/);
  await h.clickNode('new-folder');
  assert.equal(h.pending[0].args.analysisId, 'fallback-refreshed');
  assert.equal(h.pending[0].args.nodeId, 'new-folder');
  h.pending.shift().resolve(report({ analysisId: 'new-folder-opened', sourceAnalysisId: 'fixture', cachedBrowse: true, root: replacement }));
  await h.settle();
  assertSelectedLocation(h, '/example/folder');
});

test('a replaced file receives a new registered ID and can be revealed after the shallow refresh', async () => {
  const h = await harness();
  const old = node({ id: 'old-file', path: '/example/file.txt', name: 'file.txt', kind: 'file', bytes: 2000 });
  const scan = h.app.scan();
  h.pending.shift().resolve(report({ root: node({ children: [old], hasChildren: true }) }));
  await scan;
  h.queueReveal({ status: 'changed' });
  await h.clickNode('old-file');
  const replacement = { ...old, id: 'new-file', bytes: 64, sizeSource: 'stat' };
  h.pending.shift().resolve(report({ analysisId: 'file-refreshed', sourceAnalysisId: 'fixture', cachedBrowse: true, root: node({ children: [replacement], hasChildren: true }) }));
  await h.settle();
  assert.equal(h.elements.get('analysis-rows').innerHTML.includes('data-analysis-node="old-file"'), false);
  assert.equal(h.elements.get('analysis-rows').innerHTML.includes('data-analysis-node="new-file"'), true);
  assert.match(h.elements.get('analysis-message').textContent, /这个项目已变化，列表已更新/);
  await h.clickNode('new-file');
  assert.equal(h.calls.at(-1).command, 'reveal_analysis_node');
  assert.equal(h.calls.at(-1).args.analysisId, 'file-refreshed');
  assert.equal(h.calls.at(-1).args.nodeId, 'new-file');
  assert.equal(h.pending.length, 0);
});

test('permission and expired lookup errors preserve existing rows without treating them as missing', async () => {
  const h = await harness();
  const file = node({ id: 'file', path: '/example/file.txt', name: 'file.txt', kind: 'file' });
  const folder = node({ id: 'folder', path: '/example/folder', name: 'folder', children: [file], hasChildren: true });
  const scan = h.app.scan();
  h.pending.shift().resolve(report({ root: node({ children: [folder, file], hasChildren: true }) }));
  await scan;
  const rows = h.elements.get('analysis-rows').innerHTML;
  h.queueInspect(new Error('Permission denied: /example/secret/long/path'));
  await h.clickNode('folder');
  assert.equal(h.elements.get('analysis-rows').innerHTML, rows);
  assert.match(h.elements.get('analysis-message').textContent, /检查访问权限/);
  assert.equal(h.elements.get('analysis-message').textContent.includes('/example'), false);
  h.queueReveal(new Error('Permission denied: /example/file.txt'));
  await h.clickNode('file');
  assert.equal(h.elements.get('analysis-rows').innerHTML, rows);
  h.queueInspect(new Error('此分析结果已过期'));
  await h.clickNode('folder');
  assert.equal(h.elements.get('analysis-rows').innerHTML, rows);
  assert.match(h.elements.get('analysis-message').textContent, /已过期，请重新分析/);
  assert.equal(h.elements.get('analysis-message').textContent.includes('权限'), false);
  assert.equal(h.pending.length, 0);
});

test('partial permission replies keep an old list while late reveal replies cannot replace a newer view', async () => {
  const h = await harness();
  const file = node({ id: 'file', path: '/example/file.txt', name: 'file.txt', kind: 'file' });
  const pendingFolder = node({ id: 'pending', path: '/example/pending', name: 'pending', hasChildren: true });
  const loadedFolder = node({ id: 'loaded', path: '/example/loaded', name: 'loaded' });
  const scan = h.app.scan();
  h.pending.shift().resolve(report({ root: node({ children: [file, pendingFolder, loadedFolder], hasChildren: true }) }));
  await scan;
  const rows = h.elements.get('analysis-rows').innerHTML;
  await h.clickNode('pending');
  h.pending.shift().resolve(report({ analysisId: 'permission-reply', cachedBrowse: true, sourceAnalysisId: 'fixture', root: { ...pendingFolder, partial: true, children: [] }, permissionDeniedCount: 1 }));
  await h.settle();
  assert.equal(h.elements.get('analysis-rows').innerHTML, rows);
  assert.match(h.elements.get('analysis-message').textContent, /访问权限/);
  let revealReply;
  h.queueReveal(new Promise((resolve) => { revealReply = resolve; }));
  await h.clickNode('file');
  await h.clickNode('loaded');
  assertSelectedLocation(h, '/example/loaded');
  revealReply({ status: 'missing' });
  await h.settle();
  assertSelectedLocation(h, '/example/loaded');
  assert.equal(h.pending.length, 0);
  await h.back();
  assert.equal(h.elements.get('analysis-rows').innerHTML.includes('file.txt'), false);
  assert.match(h.elements.get('analysis-status').textContent, /占用需更新/);
});

test('a deleted scan root ends recovery without retries and offers a new location', async () => {
  const h = await harness();
  const scan = h.app.scan();
  h.pending.shift().resolve(report());
  await scan;
  h.queueReveal({ status: 'missing' });
  await h.revealCurrent();
  assertSelectedLocation(h, '');
  assert.equal(h.pending.length, 0);
  assert.equal(h.elements.get('start-scan').disabled, true);
  assert.match(h.elements.get('analysis-empty').innerHTML, /这个位置已无法打开.*选择其他位置/);
  h.selectLocation('/example/Documents');
  assertSelectedLocation(h, '/example/Documents');
  assert.equal(h.elements.get('start-scan').disabled, false);
  assert.equal(h.pending.length, 0);
});

test('stable missing browse errors refresh the parent without exposing native paths', async () => {
  const h = await harness();
  const folder = node({ id: 'unexpanded', path: '/example/folder', name: 'folder', hasChildren: true });
  const scan = h.app.scan();
  h.pending.shift().resolve(report({ root: node({ children: [folder], hasChildren: true }) }));
  await scan;
  await h.clickNode('unexpanded');
  h.pending.shift().reject('ANALYSIS_NODE_MISSING: /example/folder: No such file or directory');
  await h.settle();
  assert.equal(h.pending[0].command, 'browse_analysis_directory');
  assert.equal(h.pending[0].args.nodeId, 'root');
  h.pending.shift().resolve(report({ analysisId: 'parent-refresh', sourceAnalysisId: 'fixture', cachedBrowse: true, root: node() }));
  await h.settle();
  assertSelectedLocation(h, '/example');
  assert.match(h.elements.get('analysis-message').textContent, /已被移动或删除，列表已更新/);
  assert.equal(h.elements.get('analysis-message').textContent.includes('/example'), false);
  assert.equal(h.elements.get('analysis-rows').innerHTML.includes('unexpanded'), false);
});

test('recovery visits each invalid ancestor at most once and stops when all are gone', async () => {
  const h = await harness();
  const deep = node({ id: 'deep', path: '/example/folder/deep', name: 'deep' });
  const folder = node({ id: 'folder', path: '/example/folder', name: 'folder', children: [deep], hasChildren: true });
  const scan = h.app.scan();
  h.pending.shift().resolve(report({ root: node({ children: [folder], hasChildren: true }) }));
  await scan;
  await h.clickNode('folder');
  await h.clickNode('deep');
  h.queueReveal({ status: 'missing' });
  await h.revealCurrent();
  assert.equal(h.pending[0].args.nodeId, 'folder');
  h.pending.shift().reject('ANALYSIS_NODE_CHANGED: folder inode changed');
  await h.settle();
  assert.equal(h.pending[0].args.nodeId, 'root');
  h.pending.shift().reject('ANALYSIS_NODE_MISSING: root is gone');
  await h.settle();
  assertSelectedLocation(h, '');
  assert.equal(h.pending.length, 0);
  const attempts = h.calls.filter((call) => call.command === 'browse_analysis_directory');
  assert.deepEqual(attempts.slice(-2).map((call) => call.args.nodeId), ['folder', 'root']);
  assert.equal(h.elements.get('start-scan').disabled, true);
  assert.equal(h.timers.size, 0);
});

test('favorite navigation reuses history and shallow tree browsing instead of scanning everything again', async () => {
  const library = node({ id: 'library', path: '/example/Library', name: 'Library', hasChildren: true });
  const h = await harness({ favorites: [{ id: 'favorite-library', path: library.path, name: 'Library' }] });
  const scan = h.app.scan();
  h.pending.shift().resolve(report({ root: node({ children: [library], hasChildren: true }) }));
  await scan;
  const first = h.app.openFavorite('favorite-library');
  assert.equal(h.pending[0].command, 'browse_analysis_directory');
  assert.equal(h.pending[0].args.nodeId, 'library');
  h.pending.shift().resolve(report({ analysisId: 'library-browse', sourceAnalysisId: 'fixture', cachedBrowse: true, root: { ...library, hasChildren: false } }));
  assert.equal(await first, true);
  assertSelectedLocation(h, library.path);
  await h.back();
  await h.clickNode('library');
  assert.equal(h.pending.length, 0);
  const requests = h.calls.length;
  assert.equal(await h.app.openFavorite('favorite-library'), true);
  assert.equal(h.calls.length, requests + 1);
  assert.equal(h.calls.at(-1).command, 'browse_analysis_directory');
  assert.equal(h.calls.filter((call) => call.command === 'analyze_directory').length, 1);
  assert.equal(h.calls.filter((call) => call.command === 'resolve_favorite_directory').length, 0);
});

test('a first favorite visit from home opens only its immediate contents, and full analysis requires the next explicit action', async () => {
  const favorite = { id: 'favorite-documents', path: '/example/Documents', name: 'Documents' };
  const h = await harness({ favorites: [favorite], view: 'home' });
  const open = h.app.openFavorite(favorite.id);
  assert.equal(h.pending[0].command, 'open_favorite_directory');
  assert.deepEqual(Object.keys(h.pending[0].args), ['favoriteId']);
  assert.equal(h.pending[0].args.favoriteId, favorite.id);
  assert.equal(await h.app.openFavorite(favorite.id), false);
  h.pending.shift().resolve(report({ analysisId: 'favorite-documents-open', cachedBrowse: true, root: node({ id: 'documents-open', path: favorite.path, name: favorite.name, bytes: 0, sizeKnown: false }) }));
  assert.equal(await open, true);
  assert.equal(h.calls.filter((call) => call.command === 'analyze_directory').length, 0);
  assert.equal(h.elements.get('scan-button-text').textContent, '完整分析');
  const full = h.app.scan();
  assert.equal(h.pending[0].command, 'analyze_directory');
  assert.equal(h.pending[0].args.path, favorite.path);
  h.pending.shift().resolve(report({ analysisId: 'documents-analysis', root: node({ id: 'documents', path: favorite.path, name: favorite.name }) }));
  await full;
  assertSelectedLocation(h, favorite.path);
});

test('a missing favorite stays registered, while resolve failures and cancellation cannot trigger an analysis', async () => {
  const favorite = { id: 'favorite-missing', path: '/example/missing', name: 'missing' };
  const h = await harness({ favorites: [favorite], readOnlyFavorites: true });
  const missing = h.app.openFavorite(favorite.id);
  h.pending.shift().reject('FAVORITE_MISSING: /example/missing does not exist');
  assert.equal(await missing, false);
  assert.match(h.favoriteNotices.at(-1), /收藏仍保留.*侧栏移除/);
  assert.equal(h.favoriteNotices.at(-1).includes('/example'), false);
  assert.equal(h.calls.filter((call) => call.command === 'analyze_directory').length, 0);
  const cancelled = h.app.openFavorite(favorite.id);
  const reply = h.pending.shift();
  await h.app.cancel();
  reply.resolve(report({ cancelled: true, cachedBrowse: true, root: node({ path: favorite.path, name: favorite.name, bytes: 0, sizeKnown: false }) }));
  assert.equal(await cancelled, false);
  assert.equal(h.calls.filter((call) => call.command === 'analyze_directory').length, 0);
  const denied = h.app.openFavorite(favorite.id);
  h.pending.shift().reject(new Error('Permission denied /example/missing'));
  assert.equal(await denied, false);
  assert.match(h.favoriteNotices.at(-1), /访问权限/);
  assert.equal(h.favoriteNotices.at(-1).includes('删除'), false);
});

test('directory star actions pass registered IDs and do not navigate or star file rows', async () => {
  const h = await harness({ favorites: [] });
  const folder = node({ id: 'folder', path: '/example/folder', name: 'folder' });
  const file = node({ id: 'file', path: '/example/file.txt', name: 'file.txt', kind: 'file' });
  const scan = h.app.scan();
  h.pending.shift().resolve(report({ root: node({ children: [folder, file], hasChildren: true }) }));
  await scan;
  const requests = h.calls.length;
  const clickStar = (id) => h.elements.get('analysis-view').listeners.get('click')({ preventDefault() {}, stopPropagation() {}, target: { closest: (selector) => selector === '[data-analysis-favorite]' ? { dataset: { analysisFavorite: id } } : null } });
  clickStar('folder');
  await h.settle();
  assert.equal(h.favoriteCalls.length, 1);
  assert.equal(h.favoriteCalls[0].analysisId, 'fixture');
  assert.equal(h.favoriteCalls[0].nodeId, 'folder');
  assertSelectedLocation(h, '/example');
  assert.equal(h.calls.length, requests);
  clickStar('file');
  await h.settle();
  assert.equal(h.favoriteCalls.length, 1);
  assert.equal(h.elements.get('analysis-rows').innerHTML.includes('data-analysis-favorite="file"'), false);
});

test('expired favorite history falls back to an ID-based shallow open, and failed first opens restore home', async () => {
  const favorite = { id: 'favorite-root', path: '/example', name: 'example' };
  const h = await harness({ favorites: [favorite] });
  const scan = h.app.scan();
  h.pending.shift().resolve(report());
  await scan;
  h.queueInspect(new Error('此分析结果已过期'));
  const open = h.app.openFavorite(favorite.id);
  await h.settle();
  assert.equal(h.pending[0].command, 'open_favorite_directory');
  assert.equal(h.pending[0].args.favoriteId, favorite.id);
  h.pending.shift().resolve(report({ analysisId: 'favorite-new-root', cachedBrowse: true, root: node({ id: 'new-root', bytes: 0, sizeKnown: false }) }));
  assert.equal(await open, true);
  assert.equal(h.calls.filter((call) => call.command === 'analyze_directory').length, 1);
  const home = await harness({ favorites: [favorite], view: 'home' });
  const denied = home.app.openFavorite(favorite.id);
  home.pending.shift().reject('Permission denied at /example');
  assert.equal(await denied, false);
  assert.equal(home.getView(), 'home');
  assert.match(home.favoriteNotices.at(-1), /访问权限/);
  assert.equal(home.pending.length, 0);
  assert.equal(home.calls.some((call) => call.command === 'analyze_directory'), false);
});

test('entering a previously empty or expanded directory always requests its current shallow list', async () => {
  const h = await harness();
  const folder = node({ id: 'folder', path: '/example/folder', name: 'folder', children: [], hasChildren: false });
  const scan = h.app.scan(); h.pending.shift().resolve(report({ root: node({ children: [folder], hasChildren: true }) })); await scan;
  const fresh = node({ id: 'fresh', path: '/example/folder/new.txt', name: 'new.txt', kind: 'file', sizeSource: 'stat', bytes: 12345 });
  h.queueBrowse(report({ analysisId: 'fresh-empty-list', sourceAnalysisId: 'fixture', cachedBrowse: true, root: { ...folder, sizeSource: 'cached', children: [fresh], hasChildren: true } }));
  await h.clickNode('folder');
  assert.deepEqual(rowIds(h), ['fresh']);
  await h.back();
  const calls = h.calls.length;
  h.queueBrowse(report({ analysisId: 'fresh-expanded-list', sourceAnalysisId: 'fixture', cachedBrowse: true, root: { ...folder, sizeSource: 'cached', children: [], hasChildren: false } }));
  await h.clickNode('folder');
  assert.equal(h.calls.length, calls + 1);
  assert.equal(h.calls.at(-1).command, 'browse_analysis_directory');
  assert.deepEqual(rowIds(h), []);
  assert.equal(h.calls.filter(call => call.command === 'analyze_directory').length, 1);
});

test('fresh file sizes are never divided by an older cached total or subtracted into a fictitious zero', async () => {
  const h = await harness();
  const folder = node({ id: 'folder', path: '/example/folder', name: 'folder', bytes: 100, hasChildren: true });
  const scan = h.app.scan(); h.pending.shift().resolve(report({ root: node({ children: [folder], hasChildren: true }) })); await scan;
  await h.clickNode('folder');
  const file = node({ id: 'new-file', path: '/example/folder/file.txt', name: 'file.txt', kind: 'file', bytes: 1000, sizeSource: 'stat' });
  h.pending.shift().resolve(report({ analysisId: 'mixed-times', cachedBrowse: true, sourceAnalysisId: 'fixture', root: { ...folder, sizeSource: 'cached', children: [file], omittedChildren: 2 } })); await h.settle();
  const rows = h.elements.get('analysis-rows').innerHTML;
  assert.equal(rows.includes('1000.0%'), false);
  assert.match(rows, /其他未展开项目[\s\S]*analysis-size">未知/);
  assert.equal(rows.includes('analysis-size">0 B'), false);
  h.elements.get('analysis-chart-toggle').listeners.get('click')();
  assert.match(h.elements.get('analysis-chart-caption').textContent, /已有统计.*完整分析/);
});

test('partial and untyped scan snapshots cannot calibrate a later scan to a premature 99 percent', async () => {
  for (const invalid of [{ scanComplete: false, root: node({ partial: true }) }, { scanComplete: undefined }, { sourceAnalysisId: 'other-source' }]) {
    const h = await harness(); const first = h.app.scan();
    h.pending.shift().resolve(report({ scannedFiles: 1000, ...invalid })); await first;
    const next = h.app.scan(); h.emit({ estimatedPercent: 30, scannedFiles: 1000, bytesFound: 2000 });
    assert.equal(h.elements.get('analysis-progress-track')['aria-valuenow'], '30');
    h.pending.shift().resolve(report()); await next;
  }
});

test('a successful browse reply wins over a late cancel request and system capacity has one source', async () => {
  const h = await harness(); const folder = node({ id: 'folder', path: '/example/folder', name: 'folder', hasChildren: true });
  const scan = h.app.scan(); h.pending.shift().resolve(report({ availableBytes: 1000, storage: { availableBytes: 9000 }, root: node({ children: [folder], hasChildren: true }) })); await scan;
  assert.match(h.elements.get('analysis-snapshot').textContent, /系统可用 9 KB/);
  assert.equal(h.elements.get('analysis-snapshot').textContent.includes('1 KB'), false);
  await h.clickNode('folder'); const pending = h.pending.shift(); await h.app.cancel();
  pending.resolve(report({ analysisId: 'finished-before-cancel', cancelled: false, cachedBrowse: true, root: folder })); await h.settle();
  assertSelectedLocation(h, folder.path);
  assert.equal(h.elements.get('analysis-message').textContent.includes('已停止打开目录'), false);
});

test('progress announcements change only at operation boundaries and detailed coverage separates causes', async () => {
  const h = await harness(); const scan = h.app.scan();
  const announcement = h.elements.get('analysis-progress-announcement'); const start = announcement.textContent;
  h.emit({ estimatedPercent: 30, scannedFiles: 10 }); h.tick(6000);
  assert.equal(announcement.textContent, start);
  h.pending.shift().resolve(report({ directoryTimeoutCount: 2, directoryWorkerLimitCount: 1, hardLinkLimitCount: 3, blockedDirectoryCount: 3, otherErrorCount: 3 })); await scan;
  assert.match(h.elements.get('analysis-space-details').innerHTML, /系统等待超时 2.*读取名额已满 1.*硬链接占用未确认 3/);
  assert.equal(h.elements.get('analysis-space-details').innerHTML.includes('受保护目录未读取 3'), false);
  assert.notEqual(announcement.textContent, start);
});
