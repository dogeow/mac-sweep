(() => {
  'use strict';
  const app = window.macSweep;
  if (!app) return;
  const $ = (id) => document.getElementById(id);
  const invoke = (command, args) => window.__TAURI__.core.invoke(command, args);
  const state = { favorites: [], loaded: false, readOnly: false, pending: false, currentPath: '', notice: '', revision: 0 };
  const icon = (filled = false) => `<svg viewBox="0 0 24 24" fill="${filled ? 'currentColor' : 'none'}" stroke="currentColor" stroke-width="1.6" stroke-linejoin="round" aria-hidden="true"><path d="m12 3 2.8 5.7 6.3.9-4.5 4.4 1 6.2-5.6-3-5.6 3 1-6.2L3.2 9.6l6-.9Z"/></svg>`;
  const enabled = () => app.desktop && !app.demo;
  const has = (path) => state.favorites.some((favorite) => favorite.path === path);
  const get = (id) => { const favorite = state.favorites.find((item) => item.id === id); return favorite ? { ...favorite } : null; };
  function render() {
    const list = $('favorites-list');
    if (!list) return;
    const busy = app.isBusy() || state.pending || !state.loaded;
    list.innerHTML = state.favorites.map((favorite) => `<div class="favorite-row${favorite.path === state.currentPath && app.getView() === 'analysis' ? ' active' : ''}"><button class="favorite-open" data-favorite-id="${app.escapeHtml(favorite.id)}" title="${app.escapeHtml(favorite.path)}" ${busy || !enabled() ? 'disabled' : ''} ${favorite.path === state.currentPath && app.getView() === 'analysis' ? 'aria-current="location"' : ''}>${icon(true)}<span class="favorite-name">${app.escapeHtml(favorite.name)}</span></button><button class="favorite-remove" data-remove-favorite="${app.escapeHtml(favorite.id)}" title="取消收藏" aria-label="取消收藏 ${app.escapeHtml(favorite.name)}" ${busy || state.readOnly || !enabled() ? 'disabled' : ''}>×</button></div>`).join('');
    if ($('favorites-empty')) {
      $('favorites-empty').classList.toggle('hidden', state.favorites.length > 0);
      $('favorites-empty').textContent = !enabled() ? '桌面应用中可收藏目录' : state.readOnly ? '收藏记录暂时无法读取' : state.loaded ? '在目录旁点星标收藏' : '正在读取收藏…';
    }
    if ($('favorites-status')) {
      $('favorites-status').textContent = state.pending ? '正在保存…' : state.notice;
      $('favorites-status').classList.toggle('hidden', !state.pending && !state.notice);
    }
  }
  function notify() {
    render();
    window.dispatchEvent(new CustomEvent('mac-sweep-favorites'));
  }
  function validFavorite(favorite) {
    return favorite && typeof favorite.id === 'string' && favorite.id.length > 0 && typeof favorite.path === 'string' && favorite.path.startsWith('/') && !favorite.path.includes('\0') && typeof favorite.name === 'string';
  }
  function apply(result) {
    if (typeof result?.warning === 'string' && result.warning) {
      state.readOnly = true;
      state.notice = '收藏记录暂时无法读取，暂不能修改。请稍后重试。';
      return false;
    }
    const favorites = Array.isArray(result) ? result : result?.favorites;
    if (!Array.isArray(favorites) || !favorites.every(validFavorite)) throw new Error('收藏结果格式无效。');
    const ids = new Set(favorites.map((favorite) => favorite.id));
    const paths = new Set(favorites.map((favorite) => favorite.path));
    if (ids.size !== favorites.length || paths.size !== favorites.length) throw new Error('收藏结果包含重复项目。');
    state.favorites = favorites.map((favorite) => ({ id: favorite.id, path: favorite.path, name: favorite.name }));
    state.readOnly = false;
    state.notice = '';
    return true;
  }
  function saveError(error) {
    const text = typeof error === 'string' ? error : error?.message || '';
    if (/数量|容量|上限|最多|太多|过多/.test(text)) return '收藏已达到上限，请先移除不需要的收藏再试。';
    if (/FAVORITE_MISSING:|ANALYSIS_NODE_MISSING:|已删除|已移动|不存在/.test(text)) return '这个目录已被移动或删除。请更新目录列表后再收藏。';
    if (/ANALYSIS_NODE_CHANGED:|已变化|已替换|过期/.test(text)) return '这个目录的分析结果已变化，请重新分析后再收藏。';
    if (/权限|permission denied|operation not permitted/i.test(text)) return '收藏未能保存，请检查访问权限后重试。';
    return '收藏未能保存，已保留原列表。请稍后重试。';
  }
  async function save(command, args) {
    if (!enabled() || app.isBusy() || state.pending || !state.loaded || state.readOnly) return false;
    const revision = ++state.revision;
    state.pending = true;
    state.notice = '';
    notify();
    try {
      const result = await invoke(command, args);
      if (revision !== state.revision) return false;
      return apply(result);
    } catch (error) {
      if (revision === state.revision) state.notice = saveError(error);
      return false;
    } finally {
      if (revision === state.revision) { state.pending = false; notify(); }
    }
  }
  async function toggle({ analysisId, nodeId, path } = {}) {
    if (typeof path !== 'string') return false;
    const favorite = state.favorites.find((item) => item.path === path);
    if (favorite) return remove(favorite.id);
    if (typeof analysisId !== 'string' || !analysisId || typeof nodeId !== 'string' || !nodeId) return false;
    return save('add_favorite_directory', { analysisId, nodeId });
  }
  async function remove(favoriteId) {
    if (!get(favoriteId)) return false;
    return save('remove_favorite_directory', { favoriteId });
  }
  function setCurrent(path) {
    const value = path || '';
    if (state.currentPath === value) return;
    state.currentPath = value;
    render();
  }
  function note(text) { state.notice = text || (state.readOnly ? '收藏记录暂时无法读取，暂不能修改。请稍后重试。' : ''); render(); }
  async function init() {
    $('favorites-list')?.addEventListener('click', (event) => {
      if (app.isBusy() || state.pending || !state.loaded || !enabled()) return;
      const removeButton = event.target.closest('[data-remove-favorite]');
      if (removeButton) { event.preventDefault(); remove(removeButton.dataset.removeFavorite); return; }
      const openButton = event.target.closest('[data-favorite-id]');
      if (openButton) { event.preventDefault(); window.macSweepAnalyzer?.openFavorite(openButton.dataset.favoriteId); }
    });
    window.addEventListener('mac-sweep-state', render);
    window.addEventListener('mac-sweep-view', render);
    render();
    if (!enabled()) { state.loaded = true; notify(); return; }
    const revision = ++state.revision;
    try {
      const result = await invoke('get_favorite_directories');
      if (revision === state.revision) apply(result);
    } catch (_error) {
      if (revision === state.revision) { state.readOnly = true; state.notice = '收藏记录暂时无法读取，暂不能修改。请稍后重试。'; }
    } finally {
      if (revision === state.revision) { state.loaded = true; notify(); }
    }
  }
  window.macSweepFavorites = { has, get, toggle, remove, setCurrent, note, icon, isBusy: () => state.pending || !state.loaded, canEdit: () => enabled() && state.loaded && !state.pending && !state.readOnly, list: () => state.favorites.map((favorite) => ({ ...favorite })) };
  init();
})();
