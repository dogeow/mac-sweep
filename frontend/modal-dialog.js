(() => {
  'use strict';
  let active = null;
  const focusable = (dialog) => [...dialog.querySelectorAll('button:not(:disabled), input:not(:disabled), select:not(:disabled), textarea:not(:disabled), a[href], [tabindex="0"]')].filter((element) => !element.hidden);
  function focus(record) {
    const target = record.initialFocus && !record.initialFocus.disabled ? record.initialFocus : focusable(record.dialog)[0] || record.dialog;
    target.focus();
  }
  function close(dialog) {
    if (!dialog) return;
    const record = active?.dialog === dialog ? active : null;
    if (record?.native && typeof dialog.close === 'function') {
      try { dialog.close(); } catch (_error) { dialog.removeAttribute('open'); }
    } else dialog.removeAttribute('open');
    dialog.open = false;
    dialog.classList.remove('legacy-modal-dialog');
    dialog.setAttribute('aria-hidden', 'true');
    if (!record) return;
    active = null;
    record.backdrop?.remove();
    for (const item of record.background) {
      item.element.inert = item.inert;
      if (item.aria === null) item.element.removeAttribute('aria-hidden');
      else item.element.setAttribute('aria-hidden', item.aria);
    }
    if (record.returnFocus?.isConnected && !record.returnFocus.disabled) record.returnFocus.focus();
  }
  function open(dialog, { initialFocus = null, returnFocus = document.activeElement } = {}) {
    if (!dialog || (active && active.dialog !== dialog)) return false;
    if (active?.dialog === dialog) return true;
    const record = { dialog, initialFocus, returnFocus, native: false, backdrop: null, background: [] };
    try {
      dialog.removeAttribute('aria-hidden');
      if (typeof dialog.showModal === 'function' && typeof dialog.close === 'function') {
        try { dialog.showModal(); record.native = dialog.open === true; }
        catch (_error) { /* Older WebKit may expose a method without modal support. */ }
      }
      if (!record.native) {
        if (!document.body || typeof document.createElement !== 'function') return false;
        record.backdrop = document.createElement('div');
        record.backdrop.className = 'legacy-modal-backdrop';
        record.backdrop.setAttribute('aria-hidden', 'true');
        document.body.appendChild(record.backdrop);
        dialog.classList.add('legacy-modal-dialog');
        dialog.setAttribute('open', '');
        dialog.open = true;
        dialog.setAttribute('role', 'dialog');
        dialog.setAttribute('aria-modal', 'true');
        dialog.setAttribute('tabindex', '-1');
        // Hide siblings at every ancestor level, so nested dialogs do not
        // leave the rest of their containing pane available to assistive tools.
        let branch = dialog;
        while (branch.parentElement) {
          for (const element of branch.parentElement.children) {
            if (element === branch || element === record.backdrop) continue;
            record.background.push({ element, inert: Boolean(element.inert), aria: element.getAttribute('aria-hidden') });
            element.inert = true;
            element.setAttribute('aria-hidden', 'true');
          }
          if (branch.parentElement === document.body) break;
          branch = branch.parentElement;
        }
      }
      active = record;
      focus(record);
      return true;
    } catch (_error) {
      active = record;
      close(dialog);
      return false;
    }
  }
  document.addEventListener('keydown', (event) => {
    if (!active) return;
    if (event.key === 'Escape' && !active.native) {
      event.preventDefault();
      event.stopImmediatePropagation();
      const dialog = active.dialog;
      const cancel = new Event('cancel', { cancelable: true });
      if (dialog.dispatchEvent(cancel)) close(dialog);
      return;
    }
    if (event.key !== 'Tab') return;
    const items = focusable(active.dialog);
    if (!items.length) { event.preventDefault(); focus(active); return; }
    const first = items[0], last = items[items.length - 1];
    if (!active.dialog.contains(document.activeElement) || (event.shiftKey && document.activeElement === first) || (!event.shiftKey && document.activeElement === last)) {
      event.preventDefault();
      (event.shiftKey ? last : first).focus();
    }
  }, true);
  for (const name of ['pointerdown', 'mousedown', 'click']) document.addEventListener(name, (event) => {
    if (!active || active.native || active.dialog.contains(event.target)) return;
    event.preventDefault();
    event.stopImmediatePropagation();
    focus(active);
  }, true);
  document.addEventListener('focusin', (event) => { if (active && !active.native && !active.dialog.contains(event.target)) focus(active); }, true);
  window.macSweepDialogs = { open, close, isOpen: (dialog) => active?.dialog === dialog || Boolean(dialog?.open) };
})();
