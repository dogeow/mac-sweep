const test = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const vm = require('node:vm');
const cleanupModel = require('../frontend/cleanup-model.js');

const decode = (value) => value.replaceAll('&quot;', '"').replaceAll('&#39;', "'").replaceAll('&lt;', '<').replaceAll('&gt;', '>').replaceAll('&amp;', '&');
const candidate = (id, bytes) => ({ id, path: `/example/Library/Caches/App/${id}`, name: id, appName: 'App', category: 'cache', risk: 'low', bytes, files: 1, modifiedAt: 1, reason: 'Old test cache', selectedByDefault: true });
const candidates = [candidate('one', 1000), candidate('two', 2000)];
const moved = (item) => ({ id: item.id, path: item.path, bytes: item.bytes, error: null });
const failed = (item, error = '关联应用正在运行') => ({ ...moved(item), error });

async function harness() {
  const document = { activeElement: null, addEventListener() {} };
  class Element {
    constructor(id = '', classes = '') {
      this.id = id;
      this.classes = new Set(classes.split(/\s+/).filter(Boolean));
      this.classList = {
        add: (...names) => names.forEach((name) => this.classes.add(name)),
        remove: (...names) => names.forEach((name) => this.classes.delete(name)),
        contains: (name) => this.classes.has(name),
        toggle: (name, enabled) => { enabled = enabled === undefined ? !this.classes.has(name) : !!enabled; enabled ? this.classes.add(name) : this.classes.delete(name); return enabled; },
      };
      this.attrs = {};
      this.dataset = {};
      this.listeners = new Map();
      this.children = [];
      this.style = {};
      this.textContent = '';
      this.disabled = false;
      this.checked = false;
      this.open = false;
      this.value = '';
      this.isConnected = true;
    }
    set className(value) { this.classes = new Set(value.split(/\s+/).filter(Boolean)); }
    get className() { return [...this.classes].join(' '); }
    set innerHTML(value) {
      this.html = value;
      this.children = [];
      for (const tag of value.matchAll(/<(input|button|details|summary|strong)\b([^>]*)>/g)) {
        const child = new Element();
        child.tag = tag[1];
        child.disabled = /(?:^|\s)disabled(?:\s|$)/.test(tag[2]);
        child.checked = /(?:^|\s)checked(?:\s|$)/.test(tag[2]);
        child.open = /(?:^|\s)open(?:\s|$)/.test(tag[2]);
        for (const attr of tag[2].matchAll(/([a-zA-Z][\w-]*)(?:="([^"]*)")?/g)) child.setAttribute(attr[1], decode(attr[2] || ''));
        this.children.push(child);
      }
    }
    get innerHTML() { return this.html || ''; }
    setAttribute(name, value) {
      this.attrs[name] = String(value);
      if (name.startsWith('data-')) this.dataset[name.slice(5).replace(/-([a-z])/g, (_, letter) => letter.toUpperCase())] = String(value);
      if (name === 'class') this.className = value;
    }
    hasAttribute(name) { return Object.hasOwn(this.attrs, name); }
    matches(selector) {
      return selector.split(',').some((part) => {
        const trimmed = part.trim();
        if (trimmed.startsWith('.')) return this.classes.has(trimmed.slice(1));
        const attr = trimmed.match(/^\[([^\]]+)\]$/);
        return attr ? this.hasAttribute(attr[1]) : trimmed === this.tag;
      });
    }
    closest(selector) { return this.matches(selector) ? this : null; }
    querySelectorAll(selector) { return this.children.filter((child) => child.matches(selector)); }
    querySelector(selector) {
      const child = this.querySelectorAll(selector)[0];
      if (child) return child;
      if (!this.fallbacks) this.fallbacks = new Map();
      if (!this.fallbacks.has(selector)) this.fallbacks.set(selector, new Element(selector));
      return this.fallbacks.get(selector);
    }
    addEventListener(name, listener) { this.listeners.set(name, listener); }
    focus() { document.activeElement = this; }
    scrollIntoView(options) { this.lastScroll = options; }
    contains() { return false; }
    showModal() { this.open = true; }
    close() { this.open = false; }
    click(target = this) { target.focus(); return this.listeners.get('click')?.({ target, preventDefault() {} }); }
  }
  const html = fs.readFileSync(path.join(__dirname, '../frontend/index.html'), 'utf8');
  const elements = new Map();
  for (const match of html.matchAll(/<[^>]*\bid="([^"]+)"[^>]*>/g)) elements.set(match[1], new Element(match[1], match[0].match(/class="([^"]+)"/)?.[1] || ''));
  assert(elements.has('cleanup-outcome'), 'Cleanup result must have a dedicated bottom status region');
  document.getElementById = (id) => elements.get(id) || null;
  document.querySelectorAll = () => [];
  const single = new Map();
  document.querySelector = (selector) => { if (!single.has(selector)) single.set(selector, new Element(selector)); return single.get(selector); };
  elements.get('include-caches').checked = true;
  elements.get('min-age').value = '14';
  const replies = new Map();
  const calls = [];
  const events = [];
  const window = {
    location: { search: '' }, MacSweepCleanup: cleanupModel,
    addEventListener() {}, dispatchEvent: (event) => events.push(event.type),
    __TAURI__: {
      core: { invoke: async (command, args) => {
        calls.push({ command, args });
        if (replies.has(command)) return replies.get(command)(args);
        if (command === 'get_last_scan') return { scanId: 'fixture', startedAt: 1, items: candidates.map((item) => ({ ...item })), warnings: [], cancelled: false };
        if (command === 'get_disk_overview') return { totalBytes: 100000, availableBytes: 10000 };
        if (command === 'open_trash') return null;
        throw new Error(`Unexpected native command: ${command}`);
      } },
      event: { listen: async () => () => {} },
    },
  };
  const source = fs.readFileSync(path.join(__dirname, '../frontend/app.js'), 'utf8');
  vm.runInNewContext(source, { window, document, URLSearchParams, CustomEvent: class { constructor(type) { this.type = type; } } });
  await new Promise(setImmediate);
  window.macSweep.showCleanup();
  const bottom = elements.get('cleanup-outcome');
  const control = (attribute) => bottom.querySelectorAll(`[${attribute}]`)[0];
  return {
    window, elements, calls, events, bottom,
    reply: (command, handler) => replies.set(command, handler),
    control,
    clean: async (response) => {
      replies.set('clean_items', typeof response === 'function' ? response : async () => response);
      elements.get('review-cleanup').click();
      await elements.get('dialog-confirm').click();
      await new Promise(setImmediate);
    },
    clickOutcome: (attribute) => bottom.click(control(attribute)),
    settle: () => new Promise(setImmediate),
  };
}

test('confirmed cleanup appears only at the bottom, with records opened on demand', async () => {
  const h = await harness();
  await h.clean({ moved: candidates.map(moved), failed: [], bytesMoved: 3000 });
  assert.equal(h.bottom.classList.contains('hidden'), false);
  assert.equal(h.bottom.classList.contains('success'), true);
  assert.match(h.bottom.innerHTML, /已移到废纸篓 2 项 · 3 KB/);
  assert.equal(h.bottom.innerHTML.includes('已释放'), false);
  assert.equal(h.elements.get('message-panel').classList.contains('hidden'), true);
  assert.equal(h.elements.get('receipt-panel').open, false);
  assert.equal(h.control('data-cleanup-view-records').disabled, false);
  assert.equal(h.control('data-cleanup-open-trash').disabled, false);
  assert.equal(h.elements.get('show-receipts').classList.contains('hidden'), true);
  h.clickOutcome('data-cleanup-view-records');
  assert.equal(h.elements.get('receipt-panel').open, true);
  assert.equal(h.elements.get('receipt-panel').lastScroll.block, 'nearest');
  h.clickOutcome('data-cleanup-open-trash');
  await h.settle();
  assert.equal(h.calls.filter((call) => call.command === 'open_trash').length, 1);
  h.window.macSweep.showHome();
  assert.equal(h.bottom.classList.contains('hidden'), true);
  h.window.macSweep.showAnalysis();
  assert.equal(h.bottom.classList.contains('hidden'), true);
  h.window.macSweep.showCleanup();
  assert.equal(h.bottom.classList.contains('hidden'), false);
  h.clickOutcome('data-cleanup-dismiss');
  assert.equal(h.bottom.classList.contains('hidden'), true);
  assert.equal(h.elements.get('show-receipts').classList.contains('hidden'), false);
  h.elements.get('receipt-panel').open = false;
  h.elements.get('show-receipts').click();
  assert.equal(h.elements.get('receipt-panel').open, true);
  assert(h.events.includes('mac-sweep-state'));
});

test('partial failure retains failed selection and shows exact native counts', async () => {
  const h = await harness();
  await h.clean({ moved: [moved(candidates[0])], failed: [failed(candidates[1])], bytesMoved: 1000 });
  assert.equal(h.bottom.classList.contains('info'), true);
  assert.match(h.bottom.innerHTML, /已移到废纸篓 1 项 · 1 KB.*1 项未完成，选择已保留/);
  assert.equal(h.elements.get('message-panel').classList.contains('hidden'), true);
  assert.match(h.elements.get('receipt-summary').textContent, /已移动 1 项/);
  assert.equal(h.elements.get('review-cleanup').disabled, false);
  h.elements.get('review-cleanup').click();
  assert.equal(h.elements.get('dialog-count').textContent, '1 个项目');
  assert.equal(h.bottom.classList.contains('hidden'), true);
  h.elements.get('dialog-cancel').click();
  let selected;
  await h.clean(async (request) => { selected = request; return { moved: [], failed: [failed(candidates[1])], bytesMoved: 0 }; });
  assert.equal(selected.scanId, 'fixture');
  assert.deepEqual(Array.from(selected.itemIds), ['two']);
});

test('all failures have no fictitious moved bytes, no empty record action, and remain visible', async () => {
  const h = await harness();
  await h.clean({ moved: [], failed: candidates.map((item) => failed(item)), bytesMoved: 0 });
  assert.match(h.bottom.innerHTML, /2 项未完成，选择已保留/);
  assert.equal(h.bottom.innerHTML.includes('已移到废纸篓'), false);
  assert.equal(h.control('data-cleanup-view-records').disabled, true);
  assert.equal(h.control('data-cleanup-open-trash').disabled, true);
  assert.equal(h.elements.get('show-receipts').classList.contains('hidden'), true);
  h.window.macSweep.showCleanup();
  assert.equal(h.bottom.classList.contains('hidden'), false);
  h.clickOutcome('data-cleanup-dismiss');
  assert.equal(h.bottom.classList.contains('hidden'), true);
  assert.equal(h.elements.get('review-cleanup').disabled, false);
});

test('rejected or incomplete cleanup receipts remain unknown and preserve selections', async () => {
  for (const response of [async () => { throw new Error('Native connection interrupted'); }, { moved: [moved(candidates[0])], failed: [], bytesMoved: 1000 }, { moved: [moved(candidates[0]), moved(candidates[0])], failed: [], bytesMoved: 2000 }]) {
    const h = await harness();
    await h.clean(response);
    assert.equal(h.bottom.classList.contains('error'), true);
    assert.match(h.bottom.innerHTML, /清理结果未确认，请检查文件状态/);
    assert.equal(h.bottom.innerHTML.includes('原文件已保留'), false);
    assert.equal(h.bottom.innerHTML.includes('已移到废纸篓'), false);
    assert.equal(h.elements.get('message-panel').classList.contains('hidden'), true);
    assert.equal(h.elements.get('receipt-panel').classList.contains('hidden'), true);
    assert.equal(h.control('data-cleanup-view-records').disabled, true);
    assert.equal(h.control('data-cleanup-open-trash').disabled, false);
    h.elements.get('review-cleanup').click();
    assert.equal(h.elements.get('dialog-count').textContent, '2 个项目');
  }
});

test('new scans clear only the outcome; generic scan errors stay at the top and clearing records clears both', async () => {
  const h = await harness();
  await h.clean({ moved: [moved(candidates[0])], failed: [failed(candidates[1])], bytesMoved: 1000 });
  h.reply('start_scan', async () => { throw new Error('Scan unavailable'); });
  h.window.macSweep.showCleanupAndScan();
  await h.settle();
  assert.equal(h.bottom.classList.contains('hidden'), true);
  assert.equal(h.elements.get('message-panel').classList.contains('error'), true);
  assert.match(h.elements.get('message-panel').innerHTML, /检查未能完成/);
  assert.match(h.elements.get('receipt-summary').textContent, /已移动 1 项/);
  await h.clean({ moved: [], failed: [failed(candidates[1])], bytesMoved: 0 });
  assert.equal(h.control('data-cleanup-view-records').disabled, false);
  h.elements.get('clear-receipts').click();
  assert.equal(h.bottom.classList.contains('hidden'), true);
  assert.equal(h.elements.get('receipt-panel').classList.contains('hidden'), true);
  assert.equal(h.elements.get('show-receipts').classList.contains('hidden'), true);
});

test('pending cleanup hides its old result and disables the fallback record action', async () => {
  const h = await harness();
  await h.clean({ moved: [moved(candidates[0])], failed: [failed(candidates[1])], bytesMoved: 1000 });
  let finish;
  const pending = h.clean(() => new Promise((resolve) => { finish = resolve; }));
  await h.settle();
  assert.equal(h.bottom.classList.contains('hidden'), true);
  assert.equal(h.elements.get('show-receipts').classList.contains('hidden'), false);
  assert.equal(h.elements.get('show-receipts').disabled, true);
  assert.equal(h.elements.get('review-cleanup').disabled, true);
  h.elements.get('show-receipts').click();
  assert.equal(h.elements.get('receipt-panel').open, false);
  finish({ moved: [], failed: [failed(candidates[1], '移动后无法验证原路径')], bytesMoved: 0 });
  await pending;
  assert.equal(h.bottom.classList.contains('hidden'), false);
  assert.match(h.bottom.innerHTML, /1 项未完成，选择已保留/);
  assert.equal(h.bottom.innerHTML.includes('原文件已保留'), false);
  assert.equal(h.elements.get('show-receipts').classList.contains('hidden'), true);
});
