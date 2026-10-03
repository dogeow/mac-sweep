(() => {
  'use strict';

  const icons = {
    sweep: '<path d="m14 3-5 9m2-2 7 4-5 7-9-5 5-6 2 1m-3 4 6 4m-3-5 2 1"/>',
    grid: '<rect x="3" y="3" width="7" height="7" rx="2"/><rect x="14" y="3" width="7" height="7" rx="2"/><rect x="3" y="14" width="7" height="7" rx="2"/><rect x="14" y="14" width="7" height="7" rx="2"/>',
    layers: '<path d="m12 3 9 5-9 5-9-5 9-5Zm-9 9 9 5 9-5M3 16l9 5 9-5"/>',
    file: '<path d="M14 2H6a2 2 0 0 0-2 2v16a2 2 0 0 0 2 2h12a2 2 0 0 0 2-2V8Z"/><path d="M14 2v6h6M8 13h8m-8 4h6"/>',
    download: '<path d="M12 3v12m-5-5 5 5 5-5M4 16v4a1 1 0 0 0 1 1h14a1 1 0 0 0 1-1v-4"/>',
    box: '<path d="m12 3 9 5v9l-9 5-9-5V8l9-5Zm0 10 9-5M12 13 3 8m9 5v9M7.5 5.5l9 5"/>',
    shield: '<path d="M12 3 4 6v6c0 5 8 9 8 9s8-4 8-9V6l-8-3Z"/><path d="m8.5 12 2.5 2.5 4.5-5"/>',
    trash: '<path d="M3 6h18M9 6V3h6v3M5 6l1 15h12l1-15M10 10v7m4-7v7"/>',
    'arrow-up-right': '<path d="M7 17 17 7M7 7h10v10"/>',
    'arrow-right': '<path d="M4 12h16m-6-6 6 6-6 6"/>',
    scan: '<path d="M8 3H5a2 2 0 0 0-2 2v3m13-5h3a2 2 0 0 1 2 2v3M3 16v3a2 2 0 0 0 2 2h3m8 0h3a2 2 0 0 0 2-2v-3M5 12h14"/>',
    drive: '<rect x="3" y="4" width="18" height="16" rx="3"/><path d="M3 14h18m-5 3h2m-7 0h.01"/>',
    folder: '<path d="M3 7V5a2 2 0 0 1 2-2h4l3 3h7a2 2 0 0 1 2 2v11a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2V7Z"/>',
    sliders: '<path d="M4 7h7m6 0h3M4 17h3m6 0h7"/><circle cx="14" cy="7" r="3"/><circle cx="10" cy="17" r="3"/>',
    'chevron-down': '<path d="m6 9 6 6 6-6"/>',
    'check-circle': '<circle cx="12" cy="12" r="9"/><path d="m8 12 3 3 5-6"/>',
    check: '<path d="m5 12 4 4L19 6"/>',
    search: '<circle cx="10.5" cy="10.5" r="7"/><path d="m16 16 5 5"/>',
    info: '<circle cx="12" cy="12" r="9"/><path d="M12 11v6m0-10h.01"/>',
    alert: '<path d="m12 3 10 18H2L12 3Z"/><path d="M12 9v5m0 3h.01"/>',
  };
  const icon = (name) => `<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.65" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">${icons[name] || icons.file}</svg>`;
  document.querySelectorAll('[data-icon]').forEach((el) => { el.innerHTML = icon(el.dataset.icon); });
  const $ = (id) => document.getElementById(id);
  const escape = (value) => String(value ?? '').replace(/[&<>"']/g, (char) => ({ '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;', "'": '&#39;' }[char]));
  const labels = { all: '所有项目', cache: '应用缓存', logs: '旧日志', installer: '安装包', orphan: '卸载后可能留下的文件' };
  const explanations = { cache: '缓存是应用暂存的内容。清理后可能需要重新下载。', logs: '旧日志是应用运行和故障的记录。清理后无法查看这些旧记录。', installer: '这些是下载的安装文件。确认应用已安装，且不再需要安装包。', orphan: '没有找到对应应用。这些文件可能含有设置、文档或账户数据，请确认不再需要。' };
  const categories = ['cache', 'logs', 'installer', 'orphan'];
  const categoryIcons = { cache: 'layers', logs: 'file', installer: 'download', orphan: 'box' };
  const desktop = typeof window.__TAURI__?.core?.invoke === 'function' && typeof window.__TAURI__?.event?.listen === 'function';
  const demo = new URLSearchParams(window.location.search).get('demo') === '1';
  const invoke = (name, args) => window.__TAURI__.core.invoke(name, args);
  const PAGE_SIZE = 100;
  const state = { view: 'home', analysisBusy: false, report: null, diskOverview: null, diskUnavailable: false, selected: new Set(), moved: new Set(), failures: new Map(), receipts: [], groups: new Map(), expanded: new Set(), filter: 'all', search: '', risk: 'all', sort: 'size-desc', page: 1, scanning: false, cancelling: false, cleaning: false, scanRevision: 0, dialogSelection: null, previousFocus: null, message: null, extraWarnings: [] };
  const bytes = (value) => {
    const number = Number(value) || 0;
    if (number === 0) return '0 B';
    const units = ['B', 'KB', 'MB', 'GB', 'TB'];
    const index = Math.min(Math.floor(Math.log(Math.max(number, 1)) / Math.log(1000)), units.length - 1);
    return `${(number / (1000 ** index)).toLocaleString('zh-CN', { maximumFractionDigits: index > 0 ? 1 : 0 })} ${units[index]}`;
  };
  const sum = (items) => items.reduce((total, item) => total + (Number(item.bytes) || 0), 0);
  const timestamp = (value) => { const raw = Number(value); return Number.isFinite(raw) && raw > 0 ? new Date(raw * 1000) : null; };
  const date = (value) => timestamp(value)?.toLocaleDateString('zh-CN', { year: 'numeric', month: '2-digit', day: '2-digit' }) || '未知';
  const activeItems = () => (state.report?.items || []).filter((item) => !state.moved.has(item.id));
  const selectedItems = () => activeItems().filter((item) => state.selected.has(item.id));
  const visibleItems = () => {
    const query = state.search.toLocaleLowerCase();
    return activeItems().filter((item) => (state.filter === 'all' || item.category === state.filter) && (state.risk === 'all' || item.risk === state.risk) && (!query || `${item.name} ${item.path} ${item.reason} ${item.appName || ''}`.toLocaleLowerCase().includes(query))).sort((a, b) => {
      if (state.sort === 'size-asc') return a.bytes - b.bytes;
      if (state.sort === 'date-asc') return (a.modifiedAt || 0) - (b.modifiedAt || 0);
      if (state.sort === 'name-asc') return String(a.name).localeCompare(String(b.name), 'zh-CN');
      return b.bytes - a.bytes;
    });
  };
  const errorText = (error) => typeof error === 'string' ? error : error?.message || '操作未能完成，请重试。';
  const currentPageItems = () => visibleItems().slice((state.page - 1) * PAGE_SIZE, state.page * PAGE_SIZE);
  function setMessage(type, title, detail = '', canOpenTrash = false) {
    state.message = { type, title, detail, canOpenTrash, context: state.view === 'analysis' ? 'analysis' : 'cleanup' };
    renderMessage();
  }
  function renderMessage() {
    const panel = $('message-panel');
    const visible = state.message && state.message.context === (state.view === 'analysis' ? 'analysis' : 'cleanup');
    panel.classList.toggle('hidden', !visible);
    if (!visible) return;
    const message = state.message;
    panel.className = `message-panel ${message.type}`;
    panel.innerHTML = `${icon(message.type === 'success' ? 'check-circle' : 'info')}<div><strong>${escape(message.title)}</strong>${message.detail ? `<p>${escape(message.detail)}</p>` : ''}</div>${message.canOpenTrash ? '<button class="text-button" data-open-trash>打开废纸篓</button>' : ''}<button class="message-dismiss" aria-label="关闭提示">×</button>`;
  }
  function renderWarnings() {
    const warnings = [...(state.report?.warnings || []), ...state.extraWarnings];
    const panel = $('warning-panel');
    panel.classList.toggle('hidden', warnings.length === 0 && !state.report?.cancelled);
    $('warning-title').textContent = state.report?.cancelled ? '检查已停止，以下是部分结果' : '检查过程中有提示';
    $('warning-summary').textContent = state.report?.cancelled ? '没有检查完所有位置；已发现的内容可以继续查看。' : `${warnings.length} 条提示，可点开查看。无法读取的内容不会被清理。`;
    $('warning-list').innerHTML = warnings.map((warning) => `<li>${escape(warning)}</li>`).join('');
    panel.querySelector('details').classList.toggle('hidden', warnings.length === 0);
  }
  function renderReceipts() {
    $('receipt-panel').classList.toggle('hidden', state.receipts.length === 0);
    $('receipt-summary').textContent = `已移动 ${state.receipts.length} 项 · 查看记录与原位置`;
    $('receipt-list').innerHTML = state.receipts.map((item) => `<li><div><strong>${escape(item.name)}</strong><span>${bytes(item.bytes)} · ${escape(item.movedAt)}</span></div><code>${escape(item.path)}</code></li>`).join('');
  }
  function renderDisk() {
    const disk = state.diskOverview || state.report?.disk;
    if (!disk || !(disk.totalBytes > 0)) {
      $('disk-description').textContent = state.diskUnavailable || !desktop ? '暂时无法读取磁盘容量' : '正在读取磁盘容量…';
      $('disk-used-label').textContent = '—';
      $('disk-free-label').textContent = '—';
      $('disk-total-label').textContent = '容量 —';
      $('disk-used-bar').style.width = '0%';
      $('disk-bar').setAttribute('aria-valuenow', '0');
      return;
    }
    const available = Math.max(0, Math.min(disk.availableBytes, disk.totalBytes));
    const used = disk.totalBytes - available;
    const percent = Math.round((used / disk.totalBytes) * 100);
    $('disk-description').textContent = state.diskUnavailable ? '上次读取的容量，暂时无法更新' : state.diskOverview ? '当前容量快照' : '上次检查时的容量';
    $('disk-used-label').textContent = `${bytes(used)} 已使用`;
    $('disk-free-label').textContent = `还剩 ${bytes(available)} 可用`;
    $('disk-total-label').textContent = `总容量 ${bytes(disk.totalBytes)}`;
    $('disk-used-bar').style.width = `${percent}%`;
    $('disk-bar').setAttribute('aria-valuenow', String(percent));
  }
  function renderHome() {
    const items = activeItems();
    const suggested = items.filter((item) => item.selectedByDefault === true);
    const manual = items.filter((item) => item.selectedByDefault !== true);
    const busy = state.scanning || state.cleaning || state.analysisBusy;
    $('home-start-scan').disabled = busy || !desktop || demo;
    $('home-scan-text').textContent = state.scanning ? '检查中…' : state.report ? '重新检查' : '开始检查';
    $('home-start-scan').classList.toggle('secondary', Boolean(state.report));
    $('home-start-scan').classList.toggle('primary', !state.report);
    $('home-view-suggestions').classList.toggle('hidden', !state.report || state.scanning);
    $('home-view-suggestions').disabled = busy;
    $('home-show-analysis').disabled = busy;
    $('home-view-suggestions').textContent = items.length ? '查看建议清理' : '查看检查结果';
    $('home-progress').classList.toggle('hidden', !state.scanning);
    $('home-cancel-scan').disabled = state.cancelling;
    $('home-cancel-scan').textContent = state.cancelling ? '正在停止…' : '停止';
    if (state.scanning) {
      $('home-result-summary').textContent = '正在检查你的 Mac';
      $('home-result-detail').textContent = '只读取文件信息。完成后，由你选择需要整理的内容。';
    } else if (!state.report) {
      $('home-result-summary').textContent = '看看有哪些文件可以整理';
      $('home-result-detail').textContent = '检查旧缓存、日志和安装包。检查不会更改你的文件。';
    } else if (!items.length) {
      $('home-result-summary').textContent = state.moved.size ? '选中的内容已移到废纸篓' : '这次没有发现建议清理的内容';
      $('home-result-detail').textContent = state.report.cancelled ? '检查提前停止，结果可能不完整。可重新检查。' : state.moved.size ? '到废纸篓检查文件，自行清空后才会释放空间。' : '你也可以查看文件夹大小，找出空间用在哪里。';
    } else {
      $('home-result-summary').textContent = suggested.length ? `建议检查 ${bytes(sum(suggested))} 的内容` : '有些内容需要你确认';
      $('home-result-detail').textContent = `${suggested.length ? '主要是旧缓存与日志。' : ''}${manual.length ? `另有 ${bytes(sum(manual))} 的安装包或应用数据，需要你确认。` : '查看说明，保留你还需要的内容。'}${state.report.cancelled ? ' 检查提前停止，这是部分结果。' : ''}`;
    }
    if (!state.scanning && state.report && ((state.report.warnings || []).length || state.extraWarnings.length)) {
      $('home-result-detail').textContent += ' 检查过程中有提示，查看检查结果了解详情。';
    }
  }
  function groupName(item) {
    if (item.appName) return item.appName;
    return { cache: '其他应用缓存', logs: '其他旧日志', installer: '下载的安装包', orphan: '未识别的应用数据' }[item.category] || '其他文件';
  }
  function renderGroups(pageItems, busy, paginated) {
    state.groups.clear();
    pageItems.forEach((item) => {
      const suggested = item.selectedByDefault === true;
      const name = groupName(item);
      const key = JSON.stringify([item.category, suggested, name]);
      if (!state.groups.has(key)) state.groups.set(key, { key, name, category: item.category, suggested, items: [] });
      state.groups.get(key).items.push(item);
    });
    const allGroups = [...state.groups.values()];
    let html = '';
    [true, false].forEach((suggested) => {
      const groups = allGroups.filter((group) => group.suggested === suggested);
      if (!groups.length) return;
      html += `<section class="suggestion-section"><div class="suggestion-section-heading"><h3>${suggested ? '建议检查' : '需要你确认'}</h3><p>${suggested ? '先看看这些旧内容是否还需要。' : '安装包和应用数据默认不选中，查看后再决定。'}</p></div>`;
      categories.forEach((category) => {
        const categoryGroups = groups.filter((group) => group.category === category);
        if (!categoryGroups.length) return;
        html += `<div class="cleanup-category"><h4>${escape(labels[category])}</h4><p>${escape(explanations[category])}</p></div>`;
        categoryGroups.forEach((group) => {
          const selected = group.items.filter((item) => state.selected.has(item.id));
          const failures = group.items.filter((item) => state.failures.has(item.id));
          const open = state.expanded.has(group.key);
          const groupCount = `${paginated ? '本页 ' : ''}${group.items.length} 项`;
          const warning = failures.length ? `<p class="group-error">${failures.length} 项未能移动，仍保留在列表中。请查看原因，并在 Finder 检查文件位置。</p>` : '';
          html += `<article class="cleanup-group${selected.length ? ' selected' : ''}${failures.length ? ' failed' : ''}"><div class="group-row"><input type="checkbox" data-group-id="${escape(group.key)}" ${selected.length === group.items.length ? 'checked' : ''} ${busy || (!suggested && !open) ? 'disabled' : ''} aria-label="选择${paginated ? '本页' : ''} ${escape(group.name)} 的 ${group.items.length} 个项目" /><span class="group-icon ${category}">${icon(categoryIcons[category])}</span><div class="group-name"><strong>${escape(group.name)}</strong><span>${escape(groupCount)}${selected.length ? ` · 已选 ${selected.length} 项` : !suggested ? ' · 查看后选择' : ''}</span></div><strong class="group-size">${bytes(sum(group.items))}</strong></div>${warning}<details class="group-details" data-group-details="${escape(group.key)}" ${open ? 'open' : ''}><summary>查看文件${!suggested && !open ? '并选择' : '详情'}</summary><div class="group-files">${group.items.map((item) => {
            const failure = state.failures.get(item.id);
            return `<div class="detail-file${failure ? ' failed' : ''}"><div class="detail-file-heading"><input type="checkbox" data-item-id="${escape(item.id)}" ${state.selected.has(item.id) ? 'checked' : ''} ${busy ? 'disabled' : ''} aria-label="选择 ${escape(item.name)}" /><strong title="${escape(item.name)}">${escape(item.name)}</strong><span>${bytes(item.bytes)}</span><button class="row-reveal" data-reveal-id="${escape(item.id)}" title="在 Finder 中显示" aria-label="在 Finder 中显示 ${escape(item.name)}" ${!desktop || demo || busy ? 'disabled' : ''}>${icon('folder')}</button></div><p class="detail-file-reason">${escape(item.reason)}</p><code class="detail-file-path">${escape(item.path)}</code><p class="detail-file-meta">修改于 ${date(item.modifiedAt)} · ${Number(item.files || 0).toLocaleString()} 个文件 · ${item.risk === 'low' ? '建议检查' : '需要你确认'}</p>${failure ? `<p class="row-error">未能移动：${escape(failure)}</p>` : ''}</div>`;
          }).join('')}</div></details></article>`;
        });
      });
      html += '</section>';
    });
    $('result-rows').innerHTML = html;
    $('result-rows').querySelectorAll('[data-group-id]').forEach((checkbox) => {
      const group = state.groups.get(checkbox.dataset.groupId);
      checkbox.indeterminate = !checkbox.checked && group.items.some((item) => state.selected.has(item.id));
    });
  }
  function render() {
    const items = activeItems();
    const selected = selectedItems();
    const visible = visibleItems();
    const pages = Math.max(1, Math.ceil(visible.length / PAGE_SIZE));
    state.page = Math.min(Math.max(1, state.page), pages);
    const pageItems = visible.slice((state.page - 1) * PAGE_SIZE, state.page * PAGE_SIZE);
    const busy = state.scanning || state.cleaning || state.analysisBusy;
    const titles = { home: '我的 Mac', cleanup: '建议清理', analysis: '空间去哪里了' };
    $('main-title').textContent = titles[state.view];
    if (state.view !== 'analysis') $('page-label').textContent = state.view === 'home' ? '先检查，再由你决定清理什么' : '选好内容后，移到废纸篓';
    ['home', 'cleanup', 'analysis'].forEach((view) => {
      $(`show-${view}`).classList.toggle('active', state.view === view);
      $(`show-${view}`).disabled = state.cleaning || (view !== state.view && (state.scanning || state.analysisBusy));
    });
    $('toggle-settings').classList.toggle('hidden', state.view !== 'cleanup');
    $('start-scan').classList.toggle('hidden', state.view === 'home');
    $('home-view').classList.toggle('hidden', state.view !== 'home');
    $('analysis-view').classList.toggle('hidden', state.view !== 'analysis');
    $('analysis-statusbar').classList.toggle('hidden', state.view !== 'analysis');
    $('cleanup-view').classList.toggle('hidden', state.view !== 'cleanup');
    $('cleanup-selection').classList.toggle('hidden', state.view !== 'cleanup');
    document.querySelectorAll('[data-category]').forEach((button) => {
      button.classList.toggle('active', button.dataset.category === state.filter);
      button.setAttribute('aria-pressed', String(button.dataset.category === state.filter));
      button.disabled = busy;
    });
    for (const category of categories) {
      const group = items.filter((item) => item.category === category);
      $(`category-${category}-bytes`).textContent = state.report ? bytes(sum(group)) : '—';
    }
    $('candidate-total').textContent = state.report ? bytes(sum(items)) : '—';
    $('selected-total').textContent = bytes(sum(selected));
    $('scan-summary-title').textContent = state.report ? (state.report.cancelled ? '检查提前停止，这是部分结果' : `检查完成 · ${date(state.report.startedAt)}`) : '先开始一次检查';
    $('scan-summary-detail').textContent = state.report ? '只展示符合检查条件的内容' : '检查不会更改文件';
    $('result-count').textContent = state.report ? `· ${bytes(sum(visible))}` : '';
    $('results-subtitle').textContent = state.report ? '先看说明，再选择不需要的内容。' : '检查完成后，会按应用和用途整理建议。';
    $('result-table').classList.toggle('hidden', visible.length === 0);
    $('empty-state').classList.toggle('hidden', visible.length !== 0);
    if (!state.report) {
      $('empty-title').textContent = state.scanning ? '正在检查' : '先检查，再查看建议';
      $('empty-description').textContent = state.scanning ? '正在读取文件信息，完成后显示结果。' : '找出旧缓存、日志和安装包，由你决定保留什么。';
    } else if (items.length === 0) {
      $('empty-title').textContent = state.moved.size > 0 ? '选中项目已移到废纸篓' : '没有发现符合条件的项目';
      $('empty-description').textContent = state.moved.size > 0 ? '请在 Finder 检查废纸篓。自行清空后才会释放磁盘空间。' : state.report.cancelled ? '检查提前停止，结果可能不完整。可以重新检查。' : '这次没有发现建议清理的内容。你也可以查看文件夹大小。';
    } else {
      $('empty-title').textContent = '没有匹配的项目';
      $('empty-description').textContent = '试试其他类别，或更短的搜索关键词。';
    }
    $('empty-scan').classList.toggle('hidden', Boolean(state.report) || state.scanning);
    $('empty-scan').disabled = busy || !desktop || demo;
    renderGroups(pageItems, busy, pages > 1);
    const all = $('select-all');
    const pageSuggested = pageItems.filter((item) => item.selectedByDefault === true);
    all.checked = pageSuggested.length > 0 && pageSuggested.every((item) => state.selected.has(item.id));
    all.indeterminate = !all.checked && pageSuggested.some((item) => state.selected.has(item.id));
    all.disabled = busy || pageSuggested.length === 0;
    all.setAttribute('aria-label', `选择本页 ${pageSuggested.length} 个建议项`);
    $('select-safe').disabled = busy || !items.some((item) => item.selectedByDefault === true);
    $('clear-selection').disabled = busy || selected.length === 0;
    $('selection-count').textContent = selected.length ? `已选 ${bytes(sum(selected))} · ${selected.length} 项` : '选择你想整理的内容';
    const reviewCount = selected.filter((item) => item.risk !== 'low').length;
    $('selection-description').textContent = selected.length ? (reviewCount ? `包含 ${reviewCount} 项需要你确认的内容` : '移到废纸篓前，会再次让你确认。') : '移到废纸篓前，会再次让你确认。';
    $('review-cleanup').disabled = busy || selected.length === 0 || !desktop || demo;
    $('review-cleanup').title = demo ? '演示模式禁止清理' : !desktop ? '请在桌面应用中使用' : '';
    $('start-scan').disabled = busy || !desktop || demo;
    $('scan-button-text').textContent = state.analysisBusy ? '查看中…' : state.scanning ? '检查中…' : state.report ? '重新检查' : '开始检查';
    $('app-status').textContent = demo ? '演示模式' : state.cleaning ? '正在清理' : state.analysisBusy ? '正在查看文件夹大小' : state.scanning ? '正在检查' : desktop ? '准备就绪' : '请打开桌面应用';
    document.querySelector('.status-dot').classList.toggle('busy', busy);
    $('open-trash').disabled = !desktop || demo;
    $('cancel-scan').disabled = state.cancelling;
    $('cancel-scan').textContent = state.cancelling ? '正在停止…' : '停止';
    $('progress-panel').classList.toggle('hidden', !state.scanning);
    const range = visible.length ? `${(state.page - 1) * PAGE_SIZE + 1}–${Math.min(state.page * PAGE_SIZE, visible.length)}` : '0';
    $('results-footer-detail').textContent = state.report ? `显示 ${range} / ${visible.length} 项${pages > 1 ? ' · 勾选本页内容' : ''}${state.moved.size ? ` · 已移动 ${state.moved.size} 项` : ''}` : '不会自动移动或删除文件';
    $('pagination').classList.toggle('hidden', pages <= 1);
    $('page-label-counter').textContent = `${state.page} / ${pages}`;
    $('previous-page').disabled = state.page <= 1;
    $('next-page').disabled = state.page >= pages;
    $('scan-settings').querySelectorAll('input, select').forEach((el) => { el.disabled = busy; });
    renderDisk();
    renderHome();
    renderWarnings();
    renderReceipts();
    renderMessage();
  }
  function applyReport(report) {
    if (!report || !Array.isArray(report.items) || typeof report.scanId !== 'string') throw new Error('扫描结果格式无效，已保留之前的结果。');
    state.report = report;
    state.selected = new Set(report.items.filter((item) => item.selectedByDefault === true).map((item) => item.id));
    state.moved.clear();
    state.failures.clear();
    state.expanded.clear();
    state.page = 1;
  }
  async function scan() {
    if (!desktop || demo || state.scanning || state.cleaning || state.analysisBusy || $('cleanup-dialog').open) return;
    const options = { includeCaches: $('include-caches').checked, includeLogs: $('include-logs').checked, includeInstallers: $('include-installers').checked, includeOrphans: $('include-orphans').checked, minAgeDays: Number($('min-age').value) };
    if (!options.includeCaches && !options.includeLogs && !options.includeInstallers && !options.includeOrphans) { setMessage('info', '至少选择一种扫描类型。', '请在扫描设置中启用要检查的文件类型。'); return; }
    const revision = ++state.scanRevision;
    state.scanning = true;
    state.cancelling = false;
    state.message = null;
    renderMessage();
    $('progress-title').textContent = '正在扫描本机文件…';
    $('progress-detail').textContent = '正在准备扫描位置';
    $('progress-stats').textContent = '';
    $('home-progress-title').textContent = '正在检查…';
    $('home-progress-detail').textContent = '正在准备读取文件';
    render();
    try {
      const report = await invoke('start_scan', { options });
      if (revision !== state.scanRevision) return;
      applyReport(report);
      refreshDisk();
      if (!report.cancelled) setMessage('success', `检查完成，找到 ${report.items.length} 项待检查内容。`, '查看说明，再选择你不需要的内容。');
    } catch (error) {
      if (revision === state.scanRevision) setMessage('error', '检查未能完成', `${errorText(error)}${state.report ? ' 已保留上次检查结果。' : ''}`);
    } finally {
      if (revision === state.scanRevision) { state.scanning = false; state.cancelling = false; render(); }
    }
  }
  async function cancelScan() {
    if (!state.scanning || state.cancelling || !desktop) return;
    const revision = state.scanRevision;
    state.cancelling = true;
    render();
    try { await invoke('cancel_scan'); } catch (error) {
      if (revision === state.scanRevision && state.scanning) { state.cancelling = false; setMessage('error', '停止扫描失败', errorText(error)); render(); }
    }
  }
  function openReview() {
    const selected = selectedItems();
    if (!desktop || demo || state.scanning || state.cleaning || !selected.length) return;
    state.dialogSelection = { scanId: state.report.scanId, itemIds: selected.map((item) => item.id) };
    state.previousFocus = document.activeElement;
    $('dialog-count').textContent = `${selected.length} 个项目`;
    $('dialog-bytes').textContent = bytes(sum(selected));
    $('dialog-categories').innerHTML = categories.map((category) => {
      const group = selected.filter((item) => item.category === category);
      return group.length ? `<div><span>${icon(categoryIcons[category])}${labels[category]}<small>${group.length} 项</small></span><strong>${bytes(sum(group))}</strong></div>` : '';
    }).join('');
    const reviewItems = selected.filter((item) => item.risk !== 'low');
    const orphanCount = selected.filter((item) => item.category === 'orphan').length;
    $('dialog-warning').classList.toggle('hidden', reviewItems.length === 0);
    $('dialog-warning').querySelector('p').textContent = orphanCount ? `包含 ${orphanCount} 项可能遗留的应用数据，可能保存设置、文档或账户。没有找到应用，不代表这些数据没用。请确认不再需要。` : `包含 ${reviewItems.length} 个需要确认的项目。安装包或工具缓存可能仍在使用，请核实后再继续。`;
    $('dialog-progress').classList.add('hidden');
    $('dialog-cancel').disabled = false;
    $('dialog-confirm').disabled = false;
    $('dialog-confirm').innerHTML = `${icon('trash')}移到废纸篓`;
    $('cleanup-dialog').showModal();
    $('dialog-cancel').focus();
  }
  function closeReview() {
    if (state.cleaning) return;
    $('cleanup-dialog').close();
    state.dialogSelection = null;
    if (state.previousFocus?.isConnected) state.previousFocus.focus();
  }
  async function cleanup() {
    const request = state.dialogSelection;
    if (!desktop || demo || !request || state.cleaning || state.scanning || request.scanId !== state.report?.scanId) return;
    state.cleaning = true;
    $('dialog-cancel').disabled = true;
    $('dialog-confirm').disabled = true;
    $('dialog-confirm').textContent = '正在移动…';
    $('dialog-progress').classList.remove('hidden');
    $('cleanup-progress-bar').style.width = '0%';
    $('cleanup-progress-text').textContent = '正在准备移到废纸篓…';
    render();
    try {
      const report = await invoke('clean_items', request);
      if (!report || !Array.isArray(report.moved) || !Array.isArray(report.failed)) throw new Error('清理结果格式无效。请在 Finder 检查文件位置后重新扫描。');
      const movedAt = new Date().toLocaleString('zh-CN', { hour12: false });
      const sourceById = new Map((state.report?.items || []).map((item) => [item.id, item]));
      report.moved.forEach((item) => { state.receipts.unshift({ path: item.path, name: sourceById.get(item.id)?.name || String(item.path).split('/').pop(), bytes: item.bytes, movedAt }); });
      report.moved.forEach((item) => { state.moved.add(item.id); state.selected.delete(item.id); state.failures.delete(item.id); });
      report.failed.forEach((item) => { state.failures.set(item.id, item.error || '移动失败'); });
      const detail = report.failed.length ? `${report.failed.length} 项未能移动，已保留选择，详情显示在列表中。废纸篓需由你在 Finder 中自行清空。` : '可在 Finder 废纸篓中找回。自行清空废纸篓后才会释放磁盘空间。';
      setMessage(report.failed.length ? 'info' : 'success', `已将 ${report.moved.length} 个项目（${bytes(report.bytesMoved)}）移到废纸篓`, detail, true);
    } catch (error) { setMessage('error', '清理未能完成', `${errorText(error)} 请检查 Finder 中的文件状态，必要时重新扫描。`); }
    finally { state.cleaning = false; closeReview(); render(); refreshDisk(); }
  }
  async function openTrash() {
    if (!desktop || demo) return;
    try { await invoke('open_trash'); } catch (error) { setMessage('error', '无法打开废纸篓', errorText(error)); }
  }
  async function revealItem(id) {
    if (!desktop || demo || !state.report || state.scanning || state.cleaning) return;
    try { await invoke('reveal_item', { scanId: state.report.scanId, itemId: id }); } catch (error) { setMessage('error', '无法显示该项目', errorText(error)); }
  }
  function scanProgress(payload) {
    if (!state.scanning) return;
    const phases = { apps: '正在识别已安装的应用', applications: '正在识别已安装的应用', cache: '正在检查应用缓存', caches: '正在检查应用缓存', logs: '正在检查历史日志', installer: '正在检查旧安装包', installers: '正在检查旧安装包', orphan: '正在查看应用留下的文件', orphans: '正在查看应用留下的文件', complete: '正在整理扫描结果' };
    $('progress-title').textContent = state.cancelling ? '正在停止扫描…' : phases[payload.phase] || '正在扫描本机文件…';
    $('progress-detail').textContent = `已检查 ${Number(payload.scannedFiles || 0).toLocaleString()} 个文件`;
    $('progress-detail').title = payload.currentPath || '';
    $('progress-stats').textContent = `${Number(payload.scannedFiles || 0).toLocaleString()} 个文件 · ${payload.foundItems || 0} 项 · ${bytes(payload.bytesFound)}`;
    $('home-progress-title').textContent = $('progress-title').textContent;
    $('home-progress-detail').textContent = `${Number(payload.scannedFiles || 0).toLocaleString()} 个文件已检查`;
  }
  function cleanupProgress(payload) {
    if (!state.cleaning) return;
    const completed = Math.max(0, Number(payload.completed) || 0);
    const total = Math.max(0, Number(payload.total) || 0);
    $('cleanup-progress-bar').style.width = `${total ? Math.min(100, (completed / total) * 100) : 0}%`;
    $('cleanup-progress-text').textContent = `${completed} / ${total} 项 · ${payload.currentPath || '正在移动文件'}`;
  }
  function closeSettings() {
    $('scan-settings').classList.add('hidden');
    $('toggle-settings').setAttribute('aria-expanded', 'false');
  }
  function setView(view) {
    if (!['home', 'cleanup', 'analysis'].includes(view) || state.cleaning || $('cleanup-dialog').open || (view !== state.view && (state.scanning || state.analysisBusy))) return false;
    state.view = view;
    closeSettings();
    render();
    window.dispatchEvent(new CustomEvent('mac-sweep-view', { detail: view }));
    return true;
  }
  function scanCurrentView() {
    if (state.view === 'analysis') window.macSweepAnalyzer?.scan(); else scan();
  }
  function cancelCurrentOperation() {
    if (state.analysisBusy) window.macSweepAnalyzer?.cancel(); else cancelScan();
  }
  function menuAction(action) {
    if ($('cleanup-dialog').open) return;
    if (action === 'home') setView('home');
    else if (action === 'analysis') setView('analysis');
    else if (action === 'scan') scanCurrentView();
    else if (action === 'cancel') cancelCurrentOperation();
    else if (action === 'settings') { if (setView('cleanup')) $('toggle-settings').click(); }
    else if (action === 'select-safe') { if (setView('cleanup')) $('select-safe').click(); }
    else if (action === 'all') { if (setView('cleanup')) { state.filter = 'all'; state.page = 1; render(); } }
    else if (action === 'open-trash') openTrash();
  }
  function bind() {
    $('start-scan').addEventListener('click', scanCurrentView);
    $('empty-scan').addEventListener('click', scan);
    $('home-start-scan').addEventListener('click', scan);
    $('home-view-suggestions').addEventListener('click', () => setView('cleanup'));
    $('home-cancel-scan').addEventListener('click', cancelScan);
    $('home-show-analysis').addEventListener('click', () => setView('analysis'));
    $('show-home').addEventListener('click', () => setView('home'));
    $('show-cleanup').addEventListener('click', () => setView('cleanup'));
    $('cancel-scan').addEventListener('click', cancelScan);
    $('open-trash').addEventListener('click', openTrash);
    $('clear-receipts').addEventListener('click', () => { state.receipts = []; renderReceipts(); });
    $('show-analysis').addEventListener('click', () => setView('analysis'));
    document.querySelector('.brand').addEventListener('click', (event) => { event.preventDefault(); setView('home'); });
    document.querySelectorAll('[data-category]').forEach((button) => button.addEventListener('click', () => { if (!setView('cleanup')) return; state.filter = button.dataset.category; state.page = 1; render(); }));
    $('toggle-settings').addEventListener('click', () => { const open = $('scan-settings').classList.toggle('hidden') === false; $('toggle-settings').setAttribute('aria-expanded', String(open)); });
    document.addEventListener('pointerdown', (event) => { if (!$('scan-settings').classList.contains('hidden') && !$('scan-settings').contains(event.target) && !$('toggle-settings').contains(event.target)) closeSettings(); });
    document.addEventListener('keydown', (event) => { if (event.key === 'Escape' && !$('scan-settings').classList.contains('hidden')) { event.preventDefault(); closeSettings(); $('toggle-settings').focus(); } });
    $('search').addEventListener('input', (event) => { state.search = event.target.value; state.page = 1; render(); });
    $('risk-filter').addEventListener('change', (event) => { state.risk = event.target.value; state.page = 1; render(); });
    $('sort').addEventListener('change', (event) => { state.sort = event.target.value; state.page = 1; render(); });
    $('previous-page').addEventListener('click', () => { if (state.page <= 1) return; state.page -= 1; render(); $('result-table').scrollIntoView({ block: 'start' }); });
    $('next-page').addEventListener('click', () => { if (state.page >= Math.ceil(visibleItems().length / PAGE_SIZE)) return; state.page += 1; render(); $('result-table').scrollIntoView({ block: 'start' }); });
    $('result-rows').addEventListener('change', (event) => {
      if (state.scanning || state.cleaning || state.analysisBusy) return;
      const checkbox = event.target.closest('[data-item-id], [data-group-id]');
      if (!checkbox) return;
      const group = checkbox.dataset.groupId ? state.groups.get(checkbox.dataset.groupId) : null;
      if (group && !group.suggested && !state.expanded.has(group.key)) return;
      const targets = group ? group.items : activeItems().filter((item) => item.id === checkbox.dataset.itemId);
      targets.forEach((item) => { if (checkbox.checked) state.selected.add(item.id); else state.selected.delete(item.id); });
      const id = checkbox.dataset.itemId || checkbox.dataset.groupId;
      const attribute = checkbox.dataset.itemId ? 'itemId' : 'groupId';
      render();
      const replacement = Array.from($('result-rows').querySelectorAll('[data-item-id], [data-group-id]')).find((el) => el.dataset[attribute] === id);
      replacement?.focus({ preventScroll: true });
    });
    $('result-rows').addEventListener('toggle', (event) => {
      const details = event.target;
      if (!details.matches('[data-group-details]')) return;
      const key = details.dataset.groupDetails;
      if (details.open) state.expanded.add(key); else state.expanded.delete(key);
      const group = state.groups.get(key);
      if (!group) return;
      const checkbox = [...$('result-rows').querySelectorAll('[data-group-id]')].find((el) => el.dataset.groupId === key);
      if (checkbox) checkbox.disabled = state.scanning || state.cleaning || state.analysisBusy || (!group.suggested && !details.open);
      details.querySelector('summary').textContent = !group.suggested && !details.open ? '查看文件并选择' : '查看文件详情';
    }, true);
    $('result-rows').addEventListener('click', (event) => { const button = event.target.closest('[data-reveal-id]'); if (button) revealItem(button.dataset.revealId); });
    $('select-all').addEventListener('change', (event) => { if (state.scanning || state.cleaning || state.analysisBusy) return; currentPageItems().filter((item) => item.selectedByDefault === true).forEach((item) => { if (event.target.checked) state.selected.add(item.id); else state.selected.delete(item.id); }); render(); });
    $('select-safe').addEventListener('click', () => { if (state.scanning || state.cleaning || state.analysisBusy) return; activeItems().filter((item) => item.selectedByDefault === true).forEach((item) => state.selected.add(item.id)); render(); });
    $('clear-selection').addEventListener('click', () => { if (state.scanning || state.cleaning || state.analysisBusy) return; state.selected.clear(); render(); });
    $('review-cleanup').addEventListener('click', openReview);
    $('dialog-cancel').addEventListener('click', closeReview);
    $('dialog-confirm').addEventListener('click', cleanup);
    $('cleanup-dialog').addEventListener('cancel', (event) => { event.preventDefault(); closeReview(); });
    $('cleanup-dialog').addEventListener('keydown', (event) => {
      if (event.key !== 'Tab' || state.cleaning) return;
      const focusable = Array.from($('cleanup-dialog').querySelectorAll('button:not(:disabled), summary, [tabindex="0"]'));
      if (!focusable.length) return;
      const first = focusable[0], last = focusable[focusable.length - 1];
      if (event.shiftKey && document.activeElement === first) { event.preventDefault(); last.focus(); }
      if (!event.shiftKey && document.activeElement === last) { event.preventDefault(); first.focus(); }
    });
    $('message-panel').addEventListener('click', (event) => {
      if (event.target.closest('[data-open-trash]')) openTrash();
      if (event.target.closest('.message-dismiss')) { state.message = null; renderMessage(); }
    });
  }
  function demoReport() {
    const now = Math.floor(Date.now() / 1000);
    const data = [
      ['preview-cache-1', '/Users/demo/Library/Caches/com.example.browser', '浏览器缓存', 'cache', 'low', 2197815296, 4632, '14 天以上未修改的应用缓存，可重新生成。', true],
      ['preview-installer-1', '/Users/demo/Downloads/DesignTool.dmg', 'DesignTool.dmg', 'installer', 'review', 981467136, 1, 'Downloads 中 42 天前的安装包，请确认无需再次使用。', false],
      ['preview-orphan-1', '/Users/demo/Library/Application Support/OldSketch', 'OldSketch', 'orphan', 'review', 732954624, 248, '未匹配到已安装应用。可能包含个人数据，需要核实。', false],
      ['preview-cache-2', '/Users/demo/Library/Caches/com.example.editor', '编辑器缓存', 'cache', 'low', 443547648, 827, '32 天以上未修改的应用缓存，可重新生成。', true],
      ['preview-logs-1', '/Users/demo/Library/Logs/DiagnosticReports/old-report.ips', 'old-report.ips', 'logs', 'low', 921600, 1, '历史诊断报告，最后修改时间早于 14 天。', true],
      ['preview-logs-2', '/Users/demo/Library/Logs/ExampleApp', 'ExampleApp 日志', 'logs', 'low', 53870592, 128, '历史运行日志，清理后将无法查看这些旧记录。', true],
    ];
    return { scanId: 'explicit-browser-demo', startedAt: now, durationMs: 4800, disk: { totalBytes: 1024 ** 4, availableBytes: 312 * (1024 ** 3) }, items: data.map(([id, path, name, category, risk, size, files, reason, selectedByDefault], index) => ({ id, path, name, category, risk, bytes: size, files, modifiedAt: now - (18 + index * 7) * 86400, reason, selectedByDefault })), warnings: ['演示提示：受保护的应用数据位置需要额外访问权限；这些位置不包含在示例结果中。'], installedAppCount: 47, scannedFiles: 68439, cancelled: false };
  }
  let diskRevision = 0;
  async function refreshDisk() {
    if (!desktop || demo) return;
    const revision = ++diskRevision;
    try {
      const disk = await invoke('get_disk_overview');
      if (revision !== diskRevision) return;
      if (!disk || !Number.isFinite(Number(disk.totalBytes)) || !(disk.totalBytes > 0) || !Number.isFinite(Number(disk.availableBytes)) || disk.availableBytes < 0) throw new Error('磁盘容量暂时无法读取');
      state.diskOverview = disk;
      state.diskUnavailable = false;
    } catch (_error) {
      if (revision !== diskRevision) return;
      state.diskUnavailable = true;
    }
    renderDisk();
  }
  async function init() {
    bind();
    window.addEventListener('focus', refreshDisk);
    if (demo) { $('demo-banner').classList.remove('hidden'); applyReport(demoReport()); render(); return; }
    render();
    if (!desktop) { setMessage('info', '请在 Mac Sweep 桌面应用中扫描', '浏览器预览不会读取你的本机文件，也无法执行清理。'); return; }
    const revision = state.scanRevision;
    const results = await Promise.allSettled([
      window.__TAURI__.event.listen('scan-progress', (event) => scanProgress(event.payload || {})),
      window.__TAURI__.event.listen('cleanup-progress', (event) => cleanupProgress(event.payload || {})),
      window.__TAURI__.event.listen('menu-action', (event) => menuAction(event.payload)),
      invoke('get_last_scan'),
      refreshDisk(),
    ]);
    for (const result of results.slice(0, 3)) {
      if (result.status === 'rejected') state.extraWarnings.push(`进度通知无法连接：${errorText(result.reason)}。操作结果仍会在完成后显示。`);
    }
    const last = results[3];
    if (revision === state.scanRevision && !state.scanning) {
      if (last.status === 'fulfilled' && last.value) { try { applyReport(last.value); } catch (error) { setMessage('error', '无法恢复上次扫描', errorText(error)); } }
      else if (last.status === 'rejected') setMessage('info', '未能读取上次扫描结果', `${errorText(last.reason)} 你可以开始一次新的扫描。`);
    }
    render();
  }
  window.macSweep = { showHome: () => setView('home'), showCleanup: () => setView('cleanup'), showCleanupAndScan: () => { if (setView('cleanup')) { state.filter = 'all'; state.page = 1; render(); scan(); } }, showAnalysis: () => setView('analysis'), getView: () => state.view, isBusy: () => state.scanning || state.cleaning || state.analysisBusy, setAnalysisBusy: (value) => { state.analysisBusy = Boolean(value); render(); }, desktop, demo, icon, formatBytes: bytes, escapeHtml: escape };
  init();
})();
