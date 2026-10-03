const test = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const vm = require('node:vm');
const source = fs.readFileSync(path.join(__dirname, '../frontend/favorites.js'), 'utf8');
const favorite = { id: 'favorite-folder', path: '/example/folder', name: 'folder' };

function backend(initial = []) {
  const state = { favorites: initial.map((item) => ({ ...item })), calls: [], nextError: null, nextReply: null };
  state.invoke = async (command, args) => {
    state.calls.push({ command, args });
    if (state.nextError) { const error = state.nextError; state.nextError = null; throw error; }
    if (state.nextReply) { const reply = state.nextReply; state.nextReply = null; return reply; }
    if (command === 'add_favorite_directory') {
      assert.deepEqual(Object.keys(args), ['analysisId', 'nodeId']);
      state.favorites.push({ ...favorite });
    } else if (command === 'remove_favorite_directory') {
      assert.deepEqual(Object.keys(args), ['favoriteId']);
      state.favorites = state.favorites.filter((item) => item.id !== args.favoriteId);
    } else assert.equal(command, 'get_favorite_directories');
    return { favorites: state.favorites.map((item) => ({ ...item })) };
  };
  return state;
}
async function session(service, { desktop = true, demo = false } = {}) {
  let busy = false;
  let view = 'analysis';
  const listeners = new Map();
  const elements = new Map();
  for (const id of ['favorites-list', 'favorites-empty', 'favorites-status']) elements.set(id, {
    textContent: '', innerHTML: '', classes: new Set(), listeners: new Map(),
    classList: { toggle(name, enabled) { const classes = elements.get(id).classes; if (enabled) classes.add(name); else classes.delete(name); } },
    addEventListener(name, handler) { this.listeners.set(name, handler); },
  });
  const opened = [];
  const app = { desktop, demo, isBusy: () => busy, getView: () => view, escapeHtml: (value) => String(value).replace(/[&<>"']/g, (character) => ({ '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;', "'": '&#39;' }[character])) };
  const window = {
    macSweep: app, macSweepAnalyzer: { openFavorite: (id) => opened.push(id) },
    __TAURI__: { core: { invoke: service.invoke } },
    addEventListener: (name, handler) => { const handlers = listeners.get(name) || []; handlers.push(handler); listeners.set(name, handlers); },
    dispatchEvent: (event) => { for (const handler of listeners.get(event.type) || []) handler(event); },
  };
  class CustomEvent { constructor(type) { this.type = type; } }
  vm.runInNewContext(source, { window, document: { getElementById: (id) => elements.get(id) }, CustomEvent });
  await new Promise(setImmediate);
  return {
    api: window.macSweepFavorites, elements, opened,
    setBusy: (value) => { busy = value; window.dispatchEvent(new CustomEvent('mac-sweep-state')); },
    setView: (value) => { view = value; window.dispatchEvent(new CustomEvent('mac-sweep-view')); },
    open: (id) => elements.get('favorites-list').listeners.get('click')({ preventDefault() {}, target: { closest: (selector) => selector === '[data-favorite-id]' ? { dataset: { favoriteId: id } } : null } }),
  };
}

test('saved favorites are obtained again on restart, and toggling removes only the stored favorite ID', async () => {
  const store = backend();
  const first = await session(store);
  assert.equal(first.api.has(favorite.path), false);
  assert.equal(await first.api.toggle({ analysisId: 'analysis', nodeId: 'folder', path: favorite.path }), true);
  assert.equal(first.api.has(favorite.path), true);
  assert.equal(first.api.icon(true).includes('fill="currentColor"'), true);
  const second = await session(store);
  assert.equal(second.api.has(favorite.path), true);
  assert.equal(store.calls.filter((call) => call.command === 'get_favorite_directories').length, 2);
  assert.equal(await second.api.toggle({ analysisId: 'unused', nodeId: 'unused', path: favorite.path }), true);
  assert.equal(second.api.has(favorite.path), false);
  assert.equal(store.calls.at(-1).command, 'remove_favorite_directory');
  assert.equal(store.calls.at(-1).args.favoriteId, favorite.id);
  assert.equal(store.calls.some((call) => /clean|trash|delete/.test(call.command)), false);
});

test('busy and saving controls refuse duplicate actions, while a late save does not change the current view', async () => {
  const store = backend([favorite]);
  const h = await session(store);
  h.api.setCurrent(favorite.path);
  assert.match(h.elements.get('favorites-list').innerHTML, /favorite-row active/);
  h.setBusy(true);
  const calls = store.calls.length;
  h.open(favorite.id);
  assert.equal(await h.api.remove(favorite.id), false);
  assert.equal(store.calls.length, calls);
  assert.equal(h.opened.length, 0);
  assert.match(h.elements.get('favorites-list').innerHTML, /data-favorite-id="favorite-folder"[^>]*disabled/);
  h.setBusy(false);
  let finish;
  store.nextReply = new Promise((resolve) => { finish = resolve; });
  const remove = h.api.remove(favorite.id);
  assert.equal(h.api.isBusy(), true);
  assert.equal(await h.api.remove(favorite.id), false);
  h.setView('home');
  h.api.setCurrent('/example/other');
  finish({ favorites: [] });
  assert.equal(await remove, true);
  assert.equal(h.api.isBusy(), false);
  assert.equal(h.elements.get('favorites-list').innerHTML.includes(' active'), false);
});

test('warning replies preserve known favorites and disable editing without preventing known-ID navigation', async () => {
  const store = backend([favorite]);
  const h = await session(store);
  store.nextReply = { favorites: [], warning: 'configuration is unreadable at /private/example' };
  assert.equal(await h.api.remove(favorite.id), false);
  assert.equal(h.api.has(favorite.path), true);
  assert.equal(h.api.canEdit(), false);
  assert.equal(h.api.isBusy(), false);
  h.api.note('');
  assert.match(h.elements.get('favorites-status').textContent, /暂不能修改/);
  assert.equal(h.elements.get('favorites-status').textContent.includes('/private'), false);
  h.open(favorite.id);
  assert.deepEqual(h.opened, [favorite.id]);
  const calls = store.calls.length;
  assert.equal(await h.api.remove(favorite.id), false);
  assert.equal(store.calls.length, calls);
  assert.match(h.elements.get('favorites-list').innerHTML, /data-remove-favorite="favorite-folder"[^>]*disabled/);
});

test('save limits and missing directories give short guidance while all failed saves retain the old list', async () => {
  const store = backend([favorite]);
  const h = await session(store);
  for (const [error, pattern] of [[new Error('收藏数量达到上限'), /达到上限.*移除/], [new Error('ANALYSIS_NODE_MISSING: /example/secret'), /已被移动或删除.*更新目录列表/], [new Error('Disk error /example/secret'), /已保留原列表.*稍后重试/]]) {
    store.nextError = error;
    assert.equal(await h.api.remove(favorite.id), false);
    assert.equal(h.api.has(favorite.path), true);
    assert.match(h.elements.get('favorites-status').textContent, pattern);
    assert.equal(h.elements.get('favorites-status').textContent.includes('/example'), false);
  }
});

test('an unreadable initial store is read-only and browser previews never invoke native favorites APIs', async () => {
  const store = backend();
  store.nextReply = { favorites: [], warning: 'corrupt' };
  const h = await session(store);
  assert.equal(h.api.canEdit(), false);
  assert.match(h.elements.get('favorites-empty').textContent, /收藏记录暂时无法读取/);
  const calls = store.calls.length;
  assert.equal(await h.api.toggle({ analysisId: 'analysis', nodeId: 'root', path: '/example' }), false);
  assert.equal(store.calls.length, calls);
  for (const options of [{ desktop: false }, { demo: true }]) {
    const previewStore = backend([favorite]);
    const preview = await session(previewStore, options);
    assert.equal(previewStore.calls.length, 0);
    assert.equal(preview.api.canEdit(), false);
    assert.equal(preview.api.has(favorite.path), false);
  }
});
