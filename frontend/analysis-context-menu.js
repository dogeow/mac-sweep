(() => {
  'use strict';
  const app = window.macSweep;
  const view = document.getElementById('analysis-view');
  const menu = document.getElementById('analysis-context-menu');
  if (!app || !view || !menu) return;
  let selection = null;
  let rowButton = null;
  let running = false;
  const buttons = () => [...menu.querySelectorAll('[data-analysis-context-action]')].filter((button) => !button.disabled);
  function restoreFocus(id, previous) {
    if (previous && previous.isConnected !== false && !previous.disabled) { previous.focus(); return; }
    if (!id) return;
    const row = [...view.querySelectorAll('[data-analysis-row]')].find((element) => element.dataset.analysisRow === id);
    const button = row?.querySelector('.analysis-node-name');
    if (button && !button.disabled) button.focus();
  }
  function close(restore = true) {
    const id = selection?.nodeId;
    const previous = rowButton;
    selection = null;
    rowButton = null;
    menu.classList.add('hidden');
    menu.setAttribute('aria-hidden', 'true');
    if (restore) restoreFocus(id, previous);
  }
  function allowed() {
    return app.desktop && !app.demo && !app.isBusy() && !running && app.getView() === 'analysis';
  }
  function show(event) {
    // Suppress WebView's Reload menu throughout this view, including blank space.
    event.preventDefault();
    close(false);
    if (!allowed()) return;
    const row = event.target.closest('[data-analysis-row]');
    if (!row || !view.contains(row)) return;
    const target = window.macSweepAnalyzer?.contextTarget?.(row.dataset.analysisRow);
    if (!target || typeof target.analysisId !== 'string' || target.nodeId !== row.dataset.analysisRow) return;
    selection = { analysisId: target.analysisId, nodeId: target.nodeId };
    rowButton = row.querySelector('.analysis-node-name');
    const trash = menu.querySelector('[data-analysis-context-action="trash"]');
    if (trash) { trash.disabled = target.canTrash !== true; trash.textContent = '移到废纸篓…'; }
    const finder = menu.querySelector('[data-analysis-context-action="finder"]');
    if (finder) finder.disabled = false;
    menu.setAttribute('role', 'menu');
    menu.setAttribute('aria-label', `${target.name || '项目'}的操作`);
    menu.setAttribute('aria-hidden', 'false');
    menu.style.position = 'fixed';
    menu.style.maxWidth = `${Math.max(0, window.innerWidth - 16)}px`;
    menu.classList.remove('hidden');
    const bounds = menu.getBoundingClientRect();
    const rowBounds = row.getBoundingClientRect();
    const keyboard = event.clientX === 0 && event.clientY === 0;
    const x = keyboard ? rowBounds.left + 24 : event.clientX;
    const y = keyboard ? rowBounds.bottom : event.clientY;
    menu.style.left = `${Math.max(8, Math.min(x, window.innerWidth - bounds.width - 8))}px`;
    menu.style.top = `${Math.max(8, Math.min(y, window.innerHeight - bounds.height - 8))}px`;
    buttons()[0]?.focus();
  }
  async function activate(action) {
    if (!selection || !allowed()) { close(); return; }
    const target = window.macSweepAnalyzer?.contextTarget?.(selection.nodeId);
    if (!target || target.analysisId !== selection.analysisId || target.nodeId !== selection.nodeId || (action === 'trash' && target.canTrash !== true)) { close(); return; }
    const handler = action === 'finder' ? window.macSweepAnalyzer?.revealTarget : action === 'trash' ? window.macSweepAnalyzer?.trashTarget : null;
    if (typeof handler !== 'function') { close(); return; }
    const id = selection.nodeId;
    const previous = rowButton;
    close(false);
    running = true;
    try {
      // The bridge owns validation, confirmation and reporting. No paths leave
      // this menu, and the Trash action opens exactly one bridge confirmation.
      await handler.call(window.macSweepAnalyzer, id);
    } catch (_error) {
      const status = document.getElementById('analysis-message');
      if (status && app.getView() === 'analysis') {
        status.className = 'analysis-message error';
        status.textContent = '暂时无法完成操作，请稍后重试。';
      }
    } finally {
      running = false;
      if (app.getView() === 'analysis' && !app.isBusy()) restoreFocus(id, previous);
    }
  }
  view.addEventListener('contextmenu', show);
  menu.addEventListener('click', (event) => {
    const button = event.target.closest('[data-analysis-context-action]');
    if (!button || !menu.contains(button) || button.disabled) return;
    event.preventDefault();
    event.stopPropagation();
    activate(button.dataset.analysisContextAction);
  });
  menu.addEventListener('keydown', (event) => {
    if (event.key === 'Escape') { event.preventDefault(); event.stopPropagation(); close(); return; }
    const items = buttons();
    if (!items.length) return;
    const current = items.indexOf(document.activeElement);
    let next;
    if (event.key === 'ArrowDown') next = (current + 1) % items.length;
    else if (event.key === 'ArrowUp') next = (current - 1 + items.length) % items.length;
    else if (event.key === 'Home') next = 0;
    else if (event.key === 'End') next = items.length - 1;
    else if (event.key === 'Tab') { close(); return; }
    else return;
    event.preventDefault();
    items[next].focus();
  });
  document.addEventListener('pointerdown', (event) => { if (selection && !menu.contains(event.target)) close(); }, true);
  document.addEventListener('scroll', () => { if (selection) close(); }, true);
  window.addEventListener('resize', () => close());
  window.addEventListener('mac-sweep-view', () => close());
  window.addEventListener('mac-sweep-state', () => { if (app.isBusy()) close(); });
  close(false);
})();
