const test = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const vm = require('node:vm');
const source = fs.readFileSync(path.join(__dirname, '../frontend/analysis-context-menu.js'), 'utf8');

function harness({ desktop = true, demo = false, canTrash = true } = {}) {
  const documentListeners = new Map();
  const windowListeners = new Map();
  const calls = [];
  let busy = false;
  let currentView = 'analysis';
  let target = { analysisId: 'analysis-original', nodeId: 'file', path: '/example/file.txt', name: 'file.txt', kind: 'file', bytes: 64, sizeKnown: true, canTrash };
  let trashAction = async () => {};
  const document = { activeElement: null, addEventListener: (name, handler) => documentListeners.set(name, handler) };
  class Element {
    constructor(type, parent = null) { this.type = type; this.parent = parent; this.dataset = {}; this.listeners = new Map(); this.classes = new Set(); this.style = {}; this.attrs = {}; this.disabled = false; this.isConnected = true; this.classList = { add: (name) => this.classes.add(name), remove: (name) => this.classes.delete(name) }; }
    setAttribute(name, value) { this.attrs[name] = String(value); }
    addEventListener(name, handler) { this.listeners.set(name, handler); }
    focus() { if (!this.disabled) document.activeElement = this; }
    contains(element) { while (element) { if (element === this) return true; element = element.parent; } return false; }
    closest(selector) {
      let element = this;
      while (element) {
        if (selector === '[data-analysis-row]' && element.dataset.analysisRow) return element;
        if (selector === '[data-analysis-context-action]' && element.dataset.analysisContextAction) return element;
        element = element.parent;
      }
      return null;
    }
    getBoundingClientRect() { return this.type === 'menu' ? { width: 180, height: 76 } : { left: 25, top: 110, bottom: 142, width: 500, height: 32 }; }
  }
  const view = new Element('view');
  const row = new Element('row', view); row.dataset.analysisRow = 'file';
  const name = new Element('name', row);
  const menu = new Element('menu');
  const finder = new Element('finder', menu); finder.dataset.analysisContextAction = 'finder';
  const trash = new Element('trash', menu); trash.dataset.analysisContextAction = 'trash';
  const message = new Element('message');
  view.querySelectorAll = () => [row];
  row.querySelector = () => name;
  menu.querySelectorAll = () => [finder, trash];
  menu.querySelector = (selector) => selector.includes('"trash"') ? trash : finder;
  const elements = new Map([['analysis-view', view], ['analysis-context-menu', menu], ['analysis-message', message]]);
  document.getElementById = (id) => elements.get(id);
  const window = {
    innerWidth: 800, innerHeight: 600,
    macSweep: { desktop, demo, isBusy: () => busy, getView: () => currentView },
    macSweepAnalyzer: {
      contextTarget: (id) => id === target?.nodeId ? { ...target } : null,
      revealTarget: async (...args) => { calls.push({ action: 'finder', args }); },
      trashTarget: async (...args) => { calls.push({ action: 'trash', args }); await trashAction(); },
    },
    addEventListener: (event, handler) => windowListeners.set(event, handler),
  };
  vm.runInNewContext(source, { window, document });
  const event = (eventTarget, overrides = {}) => ({ target: eventTarget, clientX: 100, clientY: 130, prevented: false, stopped: false, preventDefault() { this.prevented = true; }, stopPropagation() { this.stopped = true; }, ...overrides });
  return {
    view, row, name, menu, finder, trash, calls, document, message,
    show: (eventTarget = name, coordinates = {}) => { const e = event(eventTarget, coordinates); view.listeners.get('contextmenu')(e); return e; },
    click: async (button) => { const e = event(button); menu.listeners.get('click')(e); await new Promise(setImmediate); return e; },
    key: (key) => { const e = event(document.activeElement, { key }); menu.listeners.get('keydown')(e); return e; },
    outside: () => documentListeners.get('pointerdown')(event(view)),
    scroll: () => documentListeners.get('scroll')(),
    resize: () => windowListeners.get('resize')(),
    setBusy: (value) => { busy = value; windowListeners.get('mac-sweep-state')(); },
    setView: (value) => { currentView = value; windowListeners.get('mac-sweep-view')(); },
    setTarget: (value) => { target = value; },
    setTrashAction: (handler) => { trashAction = handler; },
  };
}

test('row context menus clamp to the viewport and suppress native Reload on rows and blank space', () => {
  const h = harness();
  const event = h.show(h.name, { clientX: 799, clientY: 599 });
  assert.equal(event.prevented, true);
  assert.equal(h.menu.classes.has('hidden'), false);
  assert.equal(h.menu.style.position, 'fixed');
  assert.equal(h.menu.style.left, '612px');
  assert.equal(h.menu.style.top, '516px');
  assert.equal(h.menu.attrs.role, 'menu');
  assert.strictEqual(h.document.activeElement, h.finder);
  assert.equal(h.show(h.view).prevented, true);
  assert.equal(h.menu.classes.has('hidden'), true);
});

test('finder and trash actions pass only the selected registered ID; trash invokes one bridge confirmation', async () => {
  const h = harness();
  h.show();
  await h.click(h.finder);
  assert.deepEqual(h.calls, [{ action: 'finder', args: ['file'] }]);
  assert.strictEqual(h.document.activeElement, h.name);
  let finish;
  h.setTrashAction(() => new Promise((resolve) => { finish = resolve; }));
  h.show();
  await h.click(h.trash);
  assert.equal(h.menu.classes.has('hidden'), true);
  h.show();
  assert.equal(h.menu.classes.has('hidden'), true);
  assert.equal(h.calls.filter((call) => call.action === 'trash').length, 1);
  finish();
  await new Promise(setImmediate);
  assert.strictEqual(h.document.activeElement, h.name);
});

test('protected targets, changed report identities, busy work and browser previews cannot start trash actions', async () => {
  const protectedMenu = harness({ canTrash: false });
  protectedMenu.show();
  assert.equal(protectedMenu.trash.disabled, true);
  await protectedMenu.click(protectedMenu.trash);
  assert.equal(protectedMenu.calls.length, 0);
  const changed = harness();
  changed.show();
  changed.setTarget({ analysisId: 'new-analysis', nodeId: 'file', canTrash: true });
  await changed.click(changed.trash);
  assert.equal(changed.calls.length, 0);
  changed.show();
  changed.setBusy(true);
  assert.equal(changed.menu.classes.has('hidden'), true);
  changed.show();
  assert.equal(changed.menu.classes.has('hidden'), true);
  for (const options of [{ desktop: false }, { demo: true }]) {
    const preview = harness(options);
    assert.equal(preview.show().prevented, true);
    assert.equal(preview.menu.classes.has('hidden'), true);
    assert.equal(preview.calls.length, 0);
  }
});

test('keyboard navigation skips disabled items and dismissal returns focus to the row', () => {
  const h = harness();
  h.show();
  assert.equal(h.key('ArrowDown').prevented, true);
  assert.strictEqual(h.document.activeElement, h.trash);
  h.key('Home');
  assert.strictEqual(h.document.activeElement, h.finder);
  h.key('End');
  assert.strictEqual(h.document.activeElement, h.trash);
  h.key('Escape');
  assert.equal(h.menu.classes.has('hidden'), true);
  assert.strictEqual(h.document.activeElement, h.name);
  for (const dismiss of [() => h.outside(), () => h.scroll(), () => h.resize(), () => h.setView('home')]) {
    h.setView('analysis');
    h.show();
    dismiss();
    assert.equal(h.menu.classes.has('hidden'), true);
    assert.strictEqual(h.document.activeElement, h.name);
  }
  const readonly = harness({ canTrash: false });
  readonly.show();
  readonly.key('End');
  assert.strictEqual(readonly.document.activeElement, readonly.finder);
});
