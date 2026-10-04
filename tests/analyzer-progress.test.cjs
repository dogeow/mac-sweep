const test = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const vm = require('node:vm');
const navigation = require('../frontend/analysis-navigation.js');

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
  const document = { activeElement: null, addEventListener() {} };
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
  document.getElementById = (id) => elements.get(id) || null;
  const app = {
    desktop: true, demo: false, getView: () => view, isBusy: () => busy,
    showAnalysis: () => { if (busy) return false; view = 'analysis'; windowListeners.get('mac-sweep-view')?.(); return true; },
    showHome: () => { if (busy) return false; view = 'home'; windowListeners.get('mac-sweep-view')?.(); return true; },
    setAnalysisBusy: (value) => { busy = value; },
    icon: () => '', escapeHtml: (value) => String(value).replaceAll('&', '&amp;').replaceAll('<', '&lt;').replaceAll('"', '&quot;'),
  };
  const window = {
    macSweep: app, macSweepAnalysisNavigation: navigation,
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
        if (command === 'analyze_directory' || command === 'browse_analysis_directory' || command === 'resolve_favorite_directory' || command === 'open_favorite_directory') return new Promise((resolve, reject) => pending.push({ command, args, resolve, reject }));
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
  return {
    elements, timers, pending, calls, favoriteCalls, favoriteNotices, app: window.macSweepAnalyzer, getView: () => view,
    emit: (payload) => progressListener({ payload }),
    tick: (milliseconds) => { now += milliseconds; for (const callback of [...timers.values()]) callback(); },
    unload: () => windowListeners.get('beforeunload')(),
    clickNode: async (id) => { elements.get('analysis-view').listeners.get('click')({ target: { closest: (selector) => selector === '[data-analysis-node]' ? { dataset: { analysisNode: id } } : null } }); await new Promise(setImmediate); },
    back: () => elements.get('analysis-back').listeners.get('click')(),
    breadcrumb: (index) => elements.get('analysis-breadcrumbs').listeners.get('click')({ target: { closest: () => ({ dataset: { historyIndex: String(index) } }) } }),
    selectLocation: (value) => elements.get('analysis-location').listeners.get('change')({ target: { value } }),
    settle: () => new Promise(setImmediate),
    queueInspect: (...results) => inspections.push(...results),
    queueReveal: (...results) => reveals.push(...results),
    revealCurrent: async () => { elements.get('analysis-reveal').listeners.get('click')(); await new Promise(setImmediate); },
  };
}
const node = (overrides = {}) => ({ id: 'root', path: '/example', name: 'example', kind: 'directory', bytes: 8192, files: 1, children: [], hasChildren: false, partial: false, omittedChildren: 0, ...overrides });
const report = (overrides = {}) => ({ analysisId: 'fixture', root: node(), scannedFiles: 1, warnings: [], durationMs: 1000, ...overrides });
function assertSelectedLocation(h, expected) {
  const options = [...h.elements.get('analysis-location').innerHTML.matchAll(/<option\b([^>]*)>/g)];
  const selected = options.filter((match) => /(?:^|\s)selected(?:\s|=|$)/.test(match[1])).map((match) => match[1].match(/\bvalue="([^"]*)"/)[1]);
  assert.deepEqual(selected, [expected]);
  assert.equal(h.elements.get('analysis-selected-path').textContent, expected || '选择文件夹以查看实际占用');
  assert.equal(h.elements.get('analysis-selected-path').title, expected);
}

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
  assert.equal(label.textContent, '预计 38%');
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
  assert.equal(label.textContent, '预计 38%');
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
  assert.equal(label.textContent, '准备中');
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
  h.back();
  assertSelectedLocation(h, '/example');
  assert.equal(h.elements.get('analysis-rows').innerHTML, parentRows);
  assert.equal(h.elements.get('analysis-status').textContent, parentStatus);
  const requests = h.calls.length;
  await h.clickNode('library');
  assertSelectedLocation(h, '/example/Library');
  assert.equal(h.calls.length, requests + 1);
  assert.equal(h.calls.at(-1).command, 'inspect_analysis_node');
  assert.match(h.elements.get('analysis-status').textContent, /已有统计/);
  h.elements.get('analysis-chart-toggle').listeners.get('click')();
  assert.equal(h.elements.get('analysis-chart').innerHTML.includes('data-analysis-node="cache"'), false);
  assert.match(h.elements.get('analysis-chart-caption').textContent, /尚无大小统计/);
  await h.clickNode('cache');
  assert.equal(h.pending[0].args.analysisId, 'browse-fixture');
  assert.equal(h.pending[0].args.nodeId, 'cache');
  h.pending.shift().resolve(report({ analysisId: 'deeper-browse', cachedBrowse: true, scanComplete: false, root: { ...unmeasured, children: [], hasChildren: false } }));
  await h.settle();
  assertSelectedLocation(h, '/example/Library/Caches');
  assert.match(h.elements.get('analysis-status').textContent, /待分析/);
  assert.equal(h.elements.get('analysis-warnings').classList.contains('hidden'), true);
  assert.match(h.elements.get('analysis-empty').innerHTML, /目录大小尚未统计/);
  h.breadcrumb(0);
  assertSelectedLocation(h, '/example');
  assert.equal(h.elements.get('analysis-rows').innerHTML, parentRows);
  await h.clickNode('library');
  const rescan = h.app.scan();
  const request = h.pending.shift();
  assert.equal(request.command, 'analyze_directory');
  assert.equal(request.args.path, '/example/Library');
  request.resolve(report({ analysisId: 'library-full', root: { ...library, id: 'library-full-root' } }));
  await rescan;
  assertSelectedLocation(h, '/example/Library');
  assert.match(h.elements.get('analysis-breadcrumbs').innerHTML, /data-history-index="1"/);
  h.back();
  assertSelectedLocation(h, '/example');
  assert.equal(h.elements.get('analysis-rows').innerHTML, parentRows);
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
  h.back();
  assertSelectedLocation(h, '/example/Library');
  assert.match(h.elements.get('analysis-rows').innerHTML, /Application Support/);
  await h.clickNode('support');
  h.breadcrumb(0);
  assertSelectedLocation(h, '/example');
  assert.match(h.elements.get('analysis-rows').innerHTML, /应用与系统文件/);
  assert.equal(h.calls.slice(requests).every((call) => call.command === 'inspect_analysis_node'), true);
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
  h.back();
  assertSelectedLocation(h, '/example/Library');
  h.back();
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
  assert.equal(h.elements.get('analysis-rows').innerHTML, parentRows);
  assert.match(h.elements.get('analysis-message').textContent, /已保留当前结果/);
  await h.clickNode('library');
  const cancelled = h.pending.shift();
  await h.app.cancel();
  cancelled.resolve(report({ cancelled: true, root: library }));
  await h.settle();
  assert.equal(h.elements.get('analysis-rows').innerHTML, parentRows);
  assert.equal(h.timers.size, 0);
  await h.clickNode('library');
  h.pending.shift().resolve(report({ analysisId: 'browse-old', root: { ...library, hasChildren: false } }));
  await h.settle();
  h.back();
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
  h.back();
  await h.clickNode('library');
  assert.equal(h.pending.length, 0);
  h.queueReveal({ status: 'missing' });
  await h.clickNode('removed-file');
  assert.equal(h.elements.get('analysis-rows').innerHTML.includes('removed.txt'), false);
  const refresh = h.pending.shift();
  assert.equal(refresh.command, 'browse_analysis_directory');
  assert.equal(refresh.args.analysisId, 'library-old');
  assert.equal(refresh.args.nodeId, 'library');
  refresh.resolve(report({ analysisId: 'library-refreshed', sourceAnalysisId: 'fixture', cachedBrowse: true, root: { ...library, children: [], hasChildren: false } }));
  await h.settle();
  assertSelectedLocation(h, '/example/Library');
  assert.match(h.elements.get('analysis-status').textContent, /2 MB.*占用需更新/);
  assert.match(h.elements.get('analysis-message').textContent, /已被移动或删除，列表已更新/);
  assert.equal(h.elements.get('analysis-message').classList.contains('error'), false);
  assert.equal(h.elements.get('analysis-message').textContent.includes('/example'), false);
  assert.equal(h.elements.get('scan-button-text').textContent, '重新分析');
  h.back();
  assert.match(h.elements.get('analysis-status').textContent, /3 MB.*占用需更新/);
  await h.clickNode('library');
  assert.equal(h.pending[0].command, 'browse_analysis_directory');
  assert.equal(h.pending[0].args.analysisId, 'fixture');
  h.pending.shift().resolve(report({ analysisId: 'library-new', sourceAnalysisId: 'fixture', cachedBrowse: true, root: { ...library, children: [], hasChildren: false } }));
  await h.settle();
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
  assert.equal(h.calls.filter((call) => call.command === 'browse_analysis_directory').length, 0);
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
  h.back();
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
  assert.equal(h.calls.filter((call) => call.command === 'browse_analysis_directory').length, 0);
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
  assert.deepEqual(attempts.map((call) => call.args.nodeId), ['folder', 'root']);
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
  h.back();
  await h.clickNode('library');
  assert.equal(h.pending.length, 0);
  const requests = h.calls.length;
  assert.equal(await h.app.openFavorite('favorite-library'), true);
  assert.equal(h.calls.length, requests + 1);
  assert.equal(h.calls.at(-1).command, 'inspect_analysis_node');
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
  reply.resolve(report({ cachedBrowse: true, root: node({ path: favorite.path, name: favorite.name, bytes: 0, sizeKnown: false }) }));
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
