const test = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const vm = require('node:vm');

// Simulate the dialog API available in older WebKit. These fixtures exercise
// production event handlers; they do not establish behavior on a real OS build.
function harness(mode = 'missing') {
  class FixtureEvent {
    constructor(type, options = {}) {
      Object.assign(this, { type, cancelable: false, defaultPrevented: false, immediateStopped: false }, options);
    }
    preventDefault() { if (this.cancelable) this.defaultPrevented = true; }
    stopImmediatePropagation() { this.immediateStopped = true; }
  }
  const documentListeners = new Map();
  const document = { activeElement: null };
  class Element {
    constructor(tagName) {
      this.tagName = tagName;
      this.parentElement = null;
      this.children = [];
      this.attributes = new Map();
      this.classes = new Set();
      this.listeners = new Map();
      this.inert = false;
      this.disabled = false;
      this.hidden = false;
      this.open = false;
      this.classList = {
        add: (name) => this.classes.add(name),
        remove: (name) => this.classes.delete(name),
        contains: (name) => this.classes.has(name),
      };
    }
    set className(value) { this.classes = new Set(value.split(/\s+/).filter(Boolean)); }
    get className() { return [...this.classes].join(' '); }
    get isConnected() {
      let current = this;
      while (current) { if (current === document.body) return true; current = current.parentElement; }
      return false;
    }
    appendChild(element) { element.parentElement = this; this.children.push(element); return element; }
    remove() {
      if (this.parentElement) this.parentElement.children.splice(this.parentElement.children.indexOf(this), 1);
      this.parentElement = null;
    }
    contains(element) { return element === this || this.children.some((child) => child.contains(element)); }
    setAttribute(name, value) { this.attributes.set(name, String(value)); }
    getAttribute(name) { return this.attributes.has(name) ? this.attributes.get(name) : null; }
    removeAttribute(name) { this.attributes.delete(name); }
    addEventListener(name, listener) {
      if (!this.listeners.has(name)) this.listeners.set(name, []);
      this.listeners.get(name).push(listener);
    }
    dispatchEvent(event) {
      event.target ??= this;
      for (const listener of this.listeners.get(event.type) || []) {
        listener(event);
        if (event.immediateStopped) break;
      }
      return !event.defaultPrevented;
    }
    querySelectorAll() {
      const result = [];
      const visit = (parent) => {
        for (const child of parent.children) {
          if ((['button', 'input', 'select', 'textarea'].includes(child.tagName) && !child.disabled)
            || (child.tagName === 'a' && child.getAttribute('href') !== null)
            || child.getAttribute('tabindex') === '0') result.push(child);
          visit(child);
        }
      };
      visit(this);
      return result;
    }
    focus() {
      document.activeElement = this;
      dispatchDocument('focusin', { target: this });
    }
  }
  document.body = new Element('body');
  document.createElement = (name) => new Element(name);
  document.addEventListener = (name, listener) => {
    if (!documentListeners.has(name)) documentListeners.set(name, []);
    documentListeners.get(name).push(listener);
  };
  function dispatchDocument(name, options = {}) {
    const event = new FixtureEvent(name, { cancelable: true, target: document.activeElement, ...options });
    for (const listener of documentListeners.get(name) || []) {
      listener(event);
      if (event.immediateStopped) break;
    }
    if (!event.immediateStopped) event.target?.dispatchEvent(event);
    return event;
  }
  const app = document.body.appendChild(new Element('main'));
  const opener = app.appendChild(new Element('button'));
  opener.setAttribute('aria-hidden', 'false');
  const pane = app.appendChild(new Element('section'));
  const outside = pane.appendChild(new Element('button'));
  const dialog = pane.appendChild(new Element('dialog'));
  dialog.setAttribute('aria-hidden', 'true');
  const disabled = dialog.appendChild(new Element('button'));
  disabled.disabled = true;
  const hidden = dialog.appendChild(new Element('button'));
  hidden.hidden = true;
  const first = dialog.appendChild(new Element('button'));
  const last = dialog.appendChild(new Element('button'));
  const footer = document.body.appendChild(new Element('footer'));
  footer.inert = true;
  footer.setAttribute('aria-hidden', 'true');
  let nativeOpenCalls = 0, nativeCloseCalls = 0;
  if (mode !== 'missing') dialog.showModal = () => {
    nativeOpenCalls += 1;
    if (mode === 'throwing') throw new Error('showModal unavailable in simulated WebKit');
    if (mode !== 'noop') dialog.open = true;
  };
  dialog.close = () => { nativeCloseCalls += 1; dialog.open = false; };
  document.activeElement = opener;
  const window = {};
  vm.runInNewContext(fs.readFileSync(path.join(__dirname, '../frontend/modal-dialog.js'), 'utf8'), {
    document, window, Event: FixtureEvent,
  });
  return {
    document, dialog, first, last, disabled, hidden, opener, outside, footer,
    api: window.macSweepDialogs, dispatch: dispatchDocument,
    element: (name) => new Element(name),
    nativeCalls: () => ({ open: nativeOpenCalls, close: nativeCloseCalls }),
    backdrops: () => document.body.children.filter((child) => child.classList.contains('legacy-modal-backdrop')),
  };
}

test('native dialog open/close uses browser modal APIs and restores the chosen focus', () => {
  const h = harness('native');
  assert.equal(h.api.open(h.dialog, { initialFocus: h.last }), true);
  assert.deepEqual(h.nativeCalls(), { open: 1, close: 0 });
  assert.equal(h.api.isOpen(h.dialog), true);
  assert.strictEqual(h.document.activeElement, h.last);
  assert.equal(h.backdrops().length, 0);
  assert.equal(h.opener.inert, false);
  h.api.close(h.dialog);
  assert.deepEqual(h.nativeCalls(), { open: 1, close: 1 });
  assert.equal(h.api.isOpen(h.dialog), false);
  assert.equal(h.dialog.getAttribute('aria-hidden'), 'true');
  assert.strictEqual(h.document.activeElement, h.opener);
});

for (const mode of ['missing', 'throwing', 'noop']) {
  test(`simulated old WebKit ${mode} showModal isolates every background level and restores its state`, () => {
    const h = harness(mode);
    assert.equal(h.api.open(h.dialog), true);
    assert.equal(h.dialog.open, true);
    assert.equal(h.dialog.classList.contains('legacy-modal-dialog'), true);
    assert.equal(h.dialog.getAttribute('aria-modal'), 'true');
    assert.equal(h.backdrops().length, 1);
    assert.equal(h.backdrops()[0].getAttribute('aria-hidden'), 'true');
    assert.strictEqual(h.document.activeElement, h.first);
    for (const background of [h.opener, h.outside, h.footer]) {
      assert.equal(background.inert, true);
      assert.equal(background.getAttribute('aria-hidden'), 'true');
    }
    h.api.close(h.dialog);
    assert.equal(h.dialog.open, false);
    assert.equal(h.dialog.classList.contains('legacy-modal-dialog'), false);
    assert.equal(h.backdrops().length, 0);
    assert.equal(h.opener.inert, false);
    assert.equal(h.opener.getAttribute('aria-hidden'), 'false');
    assert.equal(h.outside.inert, false);
    assert.equal(h.outside.getAttribute('aria-hidden'), null);
    assert.equal(h.footer.inert, true);
    assert.equal(h.footer.getAttribute('aria-hidden'), 'true');
    assert.strictEqual(h.document.activeElement, h.opener);
  });
}

test('fallback traps Tab in both directions and prevents focus from escaping', () => {
  const h = harness();
  h.api.open(h.dialog);
  h.last.focus();
  assert.equal(h.dispatch('keydown', { key: 'Tab' }).defaultPrevented, true);
  assert.strictEqual(h.document.activeElement, h.first);
  assert.equal(h.dispatch('keydown', { key: 'Tab', shiftKey: true }).defaultPrevented, true);
  assert.strictEqual(h.document.activeElement, h.last);
  h.outside.focus();
  assert.strictEqual(h.document.activeElement, h.first);
  h.first.focus();
  assert.equal(h.dispatch('keydown', { key: 'Tab' }).defaultPrevented, false);
  // The browser performs ordinary in-dialog traversal; only boundaries trap.
  h.api.close(h.dialog);
  h.outside.focus();
  assert.strictEqual(h.document.activeElement, h.outside);
});

test('fallback with no available controls keeps keyboard focus on the dialog', () => {
  const h = harness();
  h.first.disabled = true;
  h.last.disabled = true;
  assert.equal(h.api.open(h.dialog), true);
  assert.strictEqual(h.document.activeElement, h.dialog);
  assert.equal(h.dispatch('keydown', { key: 'Tab' }).defaultPrevented, true);
  assert.strictEqual(h.document.activeElement, h.dialog);
});

test('fallback blocks outside pointer/mouse/click actions while inside controls remain usable', () => {
  const h = harness();
  let outsideClicks = 0, insideClicks = 0;
  h.outside.addEventListener('click', () => { outsideClicks += 1; });
  h.last.addEventListener('click', () => { insideClicks += 1; });
  h.api.open(h.dialog);
  for (const type of ['pointerdown', 'mousedown', 'click']) {
    const event = h.dispatch(type, { target: h.outside });
    assert.equal(event.defaultPrevented, true);
    assert.equal(event.immediateStopped, true);
    assert.strictEqual(h.document.activeElement, h.first);
  }
  assert.equal(outsideClicks, 0);
  assert.equal(h.dispatch('click', { target: h.last }).defaultPrevented, false);
  assert.equal(insideClicks, 1);
  h.api.close(h.dialog);
  h.dispatch('click', { target: h.outside });
  assert.equal(outsideClicks, 1);
});

test('fallback Escape dispatches cancel and stays modal when a busy handler prevents cancellation', () => {
  const h = harness();
  let busy = true, cancels = 0;
  h.dialog.addEventListener('cancel', (event) => { cancels += 1; if (busy) event.preventDefault(); });
  h.api.open(h.dialog);
  const escape = h.dispatch('keydown', { key: 'Escape' });
  assert.equal(escape.defaultPrevented, true);
  assert.equal(escape.immediateStopped, true);
  assert.equal(cancels, 1);
  assert.equal(h.api.isOpen(h.dialog), true);
  assert.equal(h.backdrops().length, 1);
  assert.equal(h.outside.inert, true);
  busy = false;
  h.dispatch('keydown', { key: 'Escape' });
  assert.equal(cancels, 2);
  assert.equal(h.api.isOpen(h.dialog), false);
  assert.equal(h.backdrops().length, 0);
  assert.strictEqual(h.document.activeElement, h.opener);
});

test('failed fallback opening removes backdrop, restores background, and releases the active modal', () => {
  const h = harness();
  const failing = h.element('button');
  h.dialog.appendChild(failing);
  failing.focus = () => { throw new Error('focus failed'); };
  assert.equal(h.api.open(h.dialog, { initialFocus: failing }), false);
  assert.equal(h.api.isOpen(h.dialog), false);
  assert.equal(h.dialog.open, false);
  assert.equal(h.dialog.classList.contains('legacy-modal-dialog'), false);
  assert.equal(h.backdrops().length, 0);
  assert.equal(h.opener.inert, false);
  assert.equal(h.outside.getAttribute('aria-hidden'), null);
  assert.strictEqual(h.document.activeElement, h.opener);
  assert.equal(h.api.open(h.dialog, { initialFocus: h.first }), true);
  h.api.close(h.dialog);
});

test('missing DOM support refuses opening without an active modal and allows a later retry', () => {
  const h = harness();
  const body = h.document.body;
  h.document.body = null;
  assert.equal(h.api.open(h.dialog), false);
  assert.equal(h.api.isOpen(h.dialog), false);
  assert.equal(h.dialog.open, false);
  assert.equal(h.dialog.classList.contains('legacy-modal-dialog'), false);
  assert.equal(h.opener.inert, false);
  assert.strictEqual(h.document.activeElement, h.opener);
  h.document.body = body;
  assert.equal(h.backdrops().length, 0);
  assert.equal(h.api.open(h.dialog), true);
  h.api.close(h.dialog);
});
