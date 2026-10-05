const test = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const vm = require('node:vm');
const cleanupModel = require('../frontend/cleanup-model.js');

const decode = (value) => value.replaceAll('&quot;', '"').replaceAll('&#39;', "'").replaceAll('&lt;', '<').replaceAll('&gt;', '>').replaceAll('&amp;', '&');
const candidate = (id, bytes) => ({ id, path: `/example/Library/Caches/App/${id}`, name: id, appName: 'App', category: 'cache', risk: 'low', bytes, files: 1, modifiedAt: 1, reason: 'Old test cache', selectedByDefault: true });
const candidates = [candidate('one', 1000), candidate('two', 2000)];
const moved = (item) => ({ id: item.id, path: item.path, bytes: item.bytes, error: null, outcome: 'moved' });
const failed = (item, error = '关联应用正在运行') => ({ ...moved(item), error, outcome: 'failed' });

async function harness(options = {}) {
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
  if (Object.hasOwn(options, 'diskOverview')) {
    replies.set('get_disk_overview', typeof options.diskOverview === 'function' ? options.diskOverview : async () => options.diskOverview);
  }
  const calls = [];
  const events = [];
  const windowListeners = new Map();
  const window = {
    location: { search: '' }, MacSweepCleanup: cleanupModel,
    addEventListener: (name, listener) => windowListeners.set(name, listener), dispatchEvent: (event) => events.push(event.type),
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
    refreshDisk: async (response) => {
      replies.set('get_disk_overview', typeof response === 'function' ? response : async () => response);
      await windowListeners.get('focus')();
      await new Promise(setImmediate);
    },
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

test('home capacity uses exclusive red and orange warnings at decimal 25 GB and 50 GB boundaries', async () => {
  const threshold = 25 * 1000 ** 3;
  const warningThreshold = 50 * 1000 ** 3;
  for (const [availableBytes, expectedLow, expectedWarning, availableLabel, usedLabel, percent] of [
    [24_000_000_000, true, false, '24 GB', '76 GB', 76],
    [threshold - 1, true, false, '25 GB', '75 GB', 75],
    [threshold, false, true, '25 GB', '75 GB', 75],
    [threshold + 1, false, true, '25 GB', '75 GB', 75],
    [49_000_000_000, false, true, '49 GB', '51 GB', 51],
    [warningThreshold - 1, false, true, '50 GB', '50 GB', 50],
    [warningThreshold, false, false, '50 GB', '50 GB', 50],
    [warningThreshold + 1, false, false, '50 GB', '50 GB', 50],
  ]) {
    const disk = Object.freeze({ totalBytes: 100_000_000_000, availableBytes });
    const h = await harness({ diskOverview: disk });
    h.window.macSweep.showHome();
    const card = h.elements.get('disk-storage-card');
    assert.equal(card.classList.contains('low-space'), expectedLow);
    assert.equal(card.classList.contains('warning-space'), expectedWarning);
    assert.equal(card.classList.contains('low-space') && card.classList.contains('warning-space'), false);
    assert.equal(h.elements.get('disk-free-label').textContent, `还剩 ${availableLabel} 可用`);
    assert.equal(h.elements.get('disk-used-label').textContent, `${usedLabel} 已使用`);
    assert.equal(h.elements.get('disk-total-label').textContent, '总容量 100 GB');
    assert.equal(h.elements.get('disk-used-bar').style.width, `${percent}%`);
    assert.equal(h.elements.get('disk-bar').attrs['aria-valuenow'], String(percent));
    assert.equal(disk.availableBytes, availableBytes);
    assert.equal(disk.totalBytes, 100_000_000_000);
  }
});

test('unavailable capacity stays unknown instead of showing a false low-space warning', async () => {
  for (const reply of [null, { totalBytes: 0, availableBytes: 0 }, async () => { throw new Error('Disk unavailable'); }]) {
    const h = await harness({ diskOverview: reply });
    h.window.macSweep.showHome();
    assert.equal(h.elements.get('disk-storage-card').classList.contains('low-space'), false);
    assert.equal(h.elements.get('disk-storage-card').classList.contains('warning-space'), false);
    assert.equal(h.elements.get('disk-free-label').textContent, '—');
    assert.equal(h.elements.get('disk-used-label').textContent, '—');
    assert.equal(h.elements.get('disk-total-label').textContent, '容量 —');
    assert.equal(h.elements.get('disk-used-bar').style.width, '0%');
    assert.equal(h.elements.get('disk-bar').attrs['aria-valuenow'], '0');
  }
});

test('real capacity refreshes reset both warning classes through red orange normal and red again', async () => {
  const h = await harness({ diskOverview: { totalBytes: 100_000_000_000, availableBytes: 24_000_000_000 } });
  h.window.macSweep.showHome();
  assert.equal(h.elements.get('disk-storage-card').classList.contains('low-space'), true);
  assert.equal(h.elements.get('disk-storage-card').classList.contains('warning-space'), false);
  assert.equal(h.elements.get('disk-free-label').textContent, '还剩 24 GB 可用');
  assert.equal(h.elements.get('disk-used-bar').style.width, '76%');
  const calls = h.calls.filter((call) => call.command === 'get_disk_overview').length;
  await h.refreshDisk(Object.freeze({ totalBytes: 100_000_000_000, availableBytes: 25_000_000_000 }));
  assert.equal(h.calls.filter((call) => call.command === 'get_disk_overview').length, calls + 1);
  assert.equal(h.elements.get('disk-storage-card').classList.contains('low-space'), false);
  assert.equal(h.elements.get('disk-storage-card').classList.contains('warning-space'), true);
  assert.equal(h.elements.get('disk-free-label').textContent, '还剩 25 GB 可用');
  assert.equal(h.elements.get('disk-used-label').textContent, '75 GB 已使用');
  assert.equal(h.elements.get('disk-used-bar').style.width, '75%');
  assert.equal(h.elements.get('disk-bar').attrs['aria-valuenow'], '75');
  await h.refreshDisk(Object.freeze({ totalBytes: 100_000_000_000, availableBytes: 50_000_000_000 }));
  assert.equal(h.calls.filter((call) => call.command === 'get_disk_overview').length, calls + 2);
  assert.equal(h.elements.get('disk-storage-card').classList.contains('low-space'), false);
  assert.equal(h.elements.get('disk-storage-card').classList.contains('warning-space'), false);
  assert.equal(h.elements.get('disk-free-label').textContent, '还剩 50 GB 可用');
  assert.equal(h.elements.get('disk-used-label').textContent, '50 GB 已使用');
  assert.equal(h.elements.get('disk-used-bar').style.width, '50%');
  assert.equal(h.elements.get('disk-bar').attrs['aria-valuenow'], '50');
  await h.refreshDisk(Object.freeze({ totalBytes: 100_000_000_000, availableBytes: 24_000_000_000 }));
  assert.equal(h.calls.filter((call) => call.command === 'get_disk_overview').length, calls + 3);
  assert.equal(h.elements.get('disk-storage-card').classList.contains('low-space'), true);
  assert.equal(h.elements.get('disk-storage-card').classList.contains('warning-space'), false);
  assert.equal(h.elements.get('disk-free-label').textContent, '还剩 24 GB 可用');
  assert.equal(h.elements.get('disk-used-label').textContent, '76 GB 已使用');
  assert.equal(h.elements.get('disk-used-bar').style.width, '76%');
  assert.equal(h.elements.get('disk-bar').attrs['aria-valuenow'], '76');
});

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
    assert.equal(h.elements.get('cleanup-dialog').open, true);
    assert.equal(h.elements.get('dialog-confirm').disabled, true);
    assert.equal(h.elements.get('dialog-cancel').disabled, false);
    h.elements.get('dialog-cancel').click();
    assert.equal(h.bottom.classList.contains('error'), true);
    assert.match(h.bottom.innerHTML, /清理结果未确认，请检查文件状态/);
    assert.equal(h.bottom.innerHTML.includes('原文件已保留'), false);
    assert.equal(h.bottom.innerHTML.includes('已移到废纸篓'), false);
    assert.equal(h.elements.get('message-panel').classList.contains('hidden'), true);
    assert.equal(h.elements.get('receipt-panel').classList.contains('hidden'), true);
    assert.equal(h.control('data-cleanup-view-records').disabled, true);
    assert.equal(h.control('data-cleanup-open-trash').disabled, false);
    assert.equal(h.elements.get('review-cleanup').disabled, true);
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

test('selecting suggested items respects the current search while confirmation exposes older hidden selection', async () => {
  const h = await harness();
  h.elements.get('clear-selection').click();
  h.elements.get('search').listeners.get('input')({ target: { value: 'one' } });
  h.elements.get('select-safe').click();
  h.elements.get('review-cleanup').click();
  assert.equal(h.elements.get('dialog-count').textContent, '1 个项目');
  h.elements.get('dialog-cancel').click();
  h.elements.get('search').listeners.get('input')({ target: { value: 'two' } });
  h.elements.get('select-safe').click();
  h.elements.get('review-cleanup').click();
  assert.match(h.elements.get('dialog-count').textContent, /2 个项目.*其中 1 项不在当前筛选结果中/);
  h.elements.get('dialog-cancel').click();
  h.elements.get('search').listeners.get('input')({ target: { value: 'nothing matches' } });
  assert.equal(h.elements.get('select-safe').disabled, true);
});

test('per-item unknown movement cannot be dismissed, cleared or failed-rescanned into a retry', async () => {
  const h = await harness();
  await h.clean({ moved: [moved(candidates[0])], failed: [{ ...failed(candidates[1]), outcome: 'unknown' }], bytesMoved: 1000 });
  assert.equal(h.elements.get('cleanup-dialog').open, true);
  assert.equal(h.elements.get('dialog-confirm').disabled, true);
  h.elements.get('dialog-cancel').click();
  assert.match(h.bottom.innerHTML, /已移到废纸篓 1 项.*1 项移动结果未确认/);
  assert.equal(h.elements.get('result-rows').innerHTML.includes('1 项未能移动'), false);
  const attempts = h.calls.filter(call => call.command === 'clean_items').length;
  h.clickOutcome('data-cleanup-dismiss');
  assert.equal(h.elements.get('review-cleanup').disabled, true);
  h.elements.get('clear-receipts').click();
  assert.equal(h.elements.get('review-cleanup').disabled, true);
  h.reply('start_scan', async () => { throw new Error('cannot rescan'); });
  h.window.macSweep.showCleanupAndScan(); await h.settle();
  assert.equal(h.elements.get('review-cleanup').disabled, true);
  h.elements.get('review-cleanup').click(); h.elements.get('dialog-confirm').click(); await h.settle();
  assert.equal(h.calls.filter(call => call.command === 'clean_items').length, attempts);
  h.reply('start_scan', async () => ({ scanId: 'new-verified-scan', startedAt: 2, items: candidates, warnings: [], cancelled: false }));
  h.window.macSweep.showCleanupAndScan(); await h.settle();
  assert.equal(h.elements.get('review-cleanup').disabled, false);
});

test('missing or misplaced outcome tags are rejected rather than classified as known movement', async () => {
  for (const invalid of [{ ...moved(candidates[0]), outcome: undefined }, { ...moved(candidates[0]), outcome: 'unknown' }]) {
    const h = await harness();
    await h.clean({ moved: [invalid], failed: [failed(candidates[1])], bytesMoved: 1000 });
    assert.equal(h.elements.get('cleanup-dialog').open, true);
    assert.equal(h.elements.get('dialog-confirm').disabled, true);
    assert.equal(h.elements.get('receipt-panel').classList.contains('hidden'), true);
    h.elements.get('dialog-cancel').click();
    assert.match(h.bottom.innerHTML, /清理结果未确认/);
    assert.equal(h.bottom.innerHTML.includes('已移到废纸篓'), false);
  }
});

test('missing confirmation support leaves no executable cleanup selection', async () => {
  const h = await harness();
  h.elements.get('cleanup-dialog').showModal = undefined;
  h.elements.get('review-cleanup').click();
  h.elements.get('dialog-confirm').click(); await h.settle();
  assert.equal(h.calls.some(call => call.command === 'clean_items'), false);
  assert.match(h.elements.get('message-panel').innerHTML, /确认窗口暂时无法打开.*没有移动/);
});
