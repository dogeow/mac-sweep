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
    showModal() { this.open = true; }
    close() { this.open = false; }
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
        if (command === 'trash_analysis_node' || command === 'analyze_directory' || command === 'browse_analysis_directory' || command === 'resolve_favorite_directory' || command === 'open_favorite_directory') return new Promise((resolve, reject) => pending.push({ command, args, resolve, reject }));
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
    elements, timers, pending, calls, favoriteCalls, favoriteNotices, app: window.macSweepAnalyzer, getView: () => view, isBusy: () => busy,
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

// Only native calls are mocked: dialog selection, confirmation, recovery and
// protected-row decisions are exercised through the current production analyzer.
const targetNode = (extra = {}) => node({ id: 'target', path: '/example/Code/project/build', name: 'build', kind: 'directory', bytes: 12345, files: 3, hasChildren: true, ...extra });
async function loaded(children = [targetNode()]) {
  const h = await harness();
  const scan = h.app.scan();
  const root = node({ children, hasChildren: true });
  h.pending.shift().resolve(report({ analysisId: 'source-one', sourceAnalysisId: 'source-one', root }));
  await scan;
  return { ...h, root };
}
const trashCalls = (h) => h.calls.filter((call) => call.command === 'trash_analysis_node');
const confirm = (h) => h.elements.get('analysis-trash-confirm').listeners.get('click')();
const cancel = (h) => h.elements.get('analysis-trash-cancel').listeners.get('click')();
const escapeDialog = (h) => h.elements.get('analysis-trash-dialog').listeners.get('cancel')({ preventDefault() {} });
function refreshReply(h, children = []) {
  const refresh = h.pending.shift();
  assert.equal(refresh.command, 'browse_analysis_directory');
  assert.deepEqual(Object.keys(refresh.args).sort(), ['analysisId', 'nodeId']);
  assert.equal(refresh.args.analysisId, 'source-one');
  assert.equal(refresh.args.nodeId, 'root');
  refresh.resolve(report({ analysisId: 'refreshed-one', sourceAnalysisId: 'source-one', cachedBrowse: true, root: { ...h.root, children } }));
}

test('opening or cancelling confirmation never calls native trash', async () => {
  const h = await loaded();
  const before = h.calls.length;
  const selection = h.app.trashTarget('target');
  assert.equal(h.elements.get('analysis-trash-dialog').open, true);
  assert.equal(h.isBusy(), true);
  assert.equal(h.elements.get('analysis-trash-name').textContent, 'build');
  assert.match(h.elements.get('analysis-trash-scope').textContent, /整个文件夹.*尚未展开/);
  assert.equal(h.calls.length, before);
  cancel(h);
  assert.equal(await selection, false);
  assert.equal(h.isBusy(), false);
  assert.equal(h.elements.get('analysis-trash-dialog').open, false);
  assert.equal(trashCalls(h).length, 0);
  const again = h.app.trashTarget('target');
  escapeDialog(h);
  assert.equal(await again, false);
  assert.equal(trashCalls(h).length, 0);
});

test('confirm sends only exact registered IDs once, then refreshes the parent shallowly', async () => {
  const h = await loaded([targetNode(), targetNode({ id: 'other', path: '/example/Code/project/keep', name: 'keep' })]);
  const selection = h.app.trashTarget('target');
  assert.equal(await h.app.trashTarget('other'), false);
  const confirming = confirm(h);
  await h.settle();
  const moving = h.pending.shift();
  assert.equal(moving.command, 'trash_analysis_node');
  assert.deepEqual(Object.keys(moving.args).sort(), ['analysisId', 'nodeId']);
  assert.equal(moving.args.analysisId, 'source-one');
  assert.equal(moving.args.nodeId, 'target');
  assert.equal(h.elements.get('analysis-trash-confirm').disabled, true);
  assert.equal(h.elements.get('analysis-trash-cancel').disabled, true);
  await confirm(h);
  cancel(h);
  escapeDialog(h);
  assert.equal(h.elements.get('analysis-trash-dialog').open, true);
  assert.equal(trashCalls(h).length, 1);
  assert.equal(await h.app.scan(), undefined);
  assert.equal(h.pending.length, 0);
  moving.resolve({ status: 'moved' });
  await h.settle();
  assert.equal(await selection, true);
  refreshReply(h, [targetNode({ id: 'other', path: '/example/Code/project/keep', name: 'keep' })]);
  await confirming;
  assert.equal(trashCalls(h).length, 1);
  assert.equal(h.app.contextTarget('target'), null);
  assert.equal(h.app.contextTarget('other').nodeId, 'other');
  assert.match(h.elements.get('analysis-message').textContent, /已移到废纸篓/);
  assert.equal(h.elements.get('analysis-message').textContent.includes('已释放'), false);
  assert.equal(h.calls.filter((call) => call.command === 'analyze_directory').length, 1);
  assert.equal(h.isBusy(), false);
});

test('missing or replaced entries are not retried by path and never reported as moved', async () => {
  for (const status of ['missing', 'changed']) {
    const h = await loaded();
    const selection = h.app.trashTarget('target');
    const confirming = confirm(h);
    h.pending.shift().resolve({ status });
    await h.settle();
    assert.equal(await selection, false);
    refreshReply(h, status === 'changed' ? [targetNode({ id: 'replacement', name: 'replacement' })] : []);
    await confirming;
    assert.equal(trashCalls(h).length, 1);
    assert.equal(trashCalls(h).some((call) => Object.hasOwn(call.args, 'path')), false);
    assert.equal(h.app.contextTarget('target'), null);
    assert.equal(h.elements.get('analysis-message').textContent.includes('已移到废纸篓'), false);
    assert.equal(await h.app.trashTarget('target'), false);
    assert.equal(trashCalls(h).length, 1);
  }
});

test('home, system, Library, SSH and case aliases never open deletion confirmation', async () => {
  const protectedPaths = [
    '/example', '/System', '/example-old/data', '/example/Library', '/example/library',
    '/example/.ssh', '/example/.SSH/key', '/example/.gnupg/key', '/example/.Trash/old',
    '/example/Library/Keychains/key', '/example/library/keychains/key',
    '/System/Volumes/Data/example/Library', '/System/Volumes/Data/example/.SSH/key',
  ];
  const h = await loaded(protectedPaths.map((path, index) => targetNode({ id: `protected-${index}`, path, name: path.split('/').pop() || path })));
  for (const [index, path] of protectedPaths.entries()) {
    const target = h.app.contextTarget(`protected-${index}`);
    assert.equal(target?.canTrash, false, path);
    assert.equal(await h.app.trashTarget(`protected-${index}`), false, path);
  }
  assert.notEqual(h.elements.get('analysis-trash-dialog').open, true);
  assert.equal(trashCalls(h).length, 0);
});

test('permission errors, expired IDs and unknown replies preserve rows without claiming movement', async () => {
  for (const response of [new Error('Permission denied'), new Error('此分析结果已过期'), { status: 'uncertain' }]) {
    const h = await loaded();
    const rows = h.elements.get('analysis-rows').innerHTML;
    const selection = h.app.trashTarget('target');
    const confirming = confirm(h);
    const moving = h.pending.shift();
    if (response instanceof Error) moving.reject(response); else moving.resolve(response);
    await confirming;
    assert.equal(h.elements.get('analysis-trash-dialog').open, true);
    assert.equal(h.elements.get('analysis-trash-confirm').disabled, true);
    assert.equal(h.elements.get('analysis-trash-cancel').disabled, false);
    cancel(h);
    assert.equal(await selection, false);
    assert.equal(h.isBusy(), false);
    assert.equal(h.elements.get('analysis-trash-dialog').open, false);
    assert.equal(h.elements.get('analysis-rows').innerHTML, rows);
    assert.match(h.elements.get('analysis-message').textContent, /未能确认.*Finder/);
    assert.equal(h.elements.get('analysis-message').textContent.includes('已移到废纸篓'), false);
    assert.equal(h.elements.get('analysis-message').textContent.includes('已释放'), false);
    assert.equal(h.pending.length, 0);
    assert.equal(trashCalls(h).length, 1);
    assert.deepEqual(Object.keys(trashCalls(h)[0].args).sort(), ['analysisId', 'nodeId']);
  }
});

test('confirmation rendering failure releases the busy state and never calls native trash', async () => {
  const h = await loaded();
  h.elements.get('analysis-trash-dialog').showModal = () => { throw new Error('legacy WebKit modal unsupported'); };
  assert.equal(await h.app.trashTarget('target'), false);
  assert.equal(h.isBusy(), false);
  assert.equal(trashCalls(h).length, 0);
  assert.match(h.elements.get('analysis-message').textContent, /确认窗口.*没有移动/);
  confirm(h);
  await h.settle();
  assert.equal(trashCalls(h).length, 0);
});

test('symlink rows keep Finder available but cannot open a Trash confirmation', async () => {
  const h = await loaded([targetNode({ kind: 'symlink' })]);
  assert.equal(h.app.contextTarget('target').canTrash, false);
  assert.match(h.app.contextTarget('target').trashReason, /Finder/);
  assert.equal(await h.app.trashTarget('target'), false);
  await h.app.revealTarget('target');
  assert.equal(h.calls.at(-1).command, 'reveal_analysis_node');
});
