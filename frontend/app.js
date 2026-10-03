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
  const GROUP_PAGE_SIZE = 30;
  const cleanupModel = window.MacSweepCleanup;
  const state = { view: 'home', analysisBusy: false, report: null, diskOverview: null, diskUnavailable: false, selected: new Set(), moved: new Set(), failures: new Map(), receipts: [], cleanOutcome: null, groups: new Map(), expanded: new Set(), expandedTree: new Set(), expandedFileInfo: new Set(), treePages: new Map(), treeNodes: new Map(), filter: 'all', search: '', risk: 'all', sort: 'size-desc', page: 1, scanning: false, cancelling: false, cleaning: false, scanRevision: 0, dialogSelection: null, previousFocus: null, message: null, extraWarnings: [] };
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
  const currentPageGroups = () => cleanupModel.pageItems(cleanupModel.groupItems(visibleItems(), state.sort), state.page, GROUP_PAGE_SIZE).items;
  const currentPageItems = () => currentPageGroups().flatMap((group) => group.items);
  function setMessage(type, title, detail = '', canOpenTrash = false) {
    state.message = { type, title, detail, canOpenTrash, context: state.view === 'analysis' ? 'analysis' : 'cleanup' };
    renderMessage();
  }
  function renderMessage() {
    const panel = $('message-panel');
    const visible = state.message && !(state.message.type === 'success' && !state.message.canOpenTrash && state.report) && state.message.context === (state.view === 'analysis' ? 'analysis' : 'cleanup');
    panel.classList.toggle('hidden', !visible);
    if (!visible) return;
    const message = state.message;
    panel.className = `message-panel ${message.type}`;
    panel.innerHTML = `${icon(message.type === 'success' ? 'check-circle' : 'info')}<div><strong>${escape(message.title)}</strong>${message.detail ? `<p>${escape(message.detail)}</p>` : ''}</div>${message.canOpenTrash ? '<button class="text-button" data-open-trash>打开废纸篓</button>' : ''}<button class="message-dismiss" aria-label="关闭提示">×</button>`;
  }
  function renderCleanOutcome() {
    renderReceiptAction();
    const panel = $('cleanup-outcome');
    if (!panel) return;
    const outcome = state.cleanOutcome;
    const busy = state.scanning || state.cleaning || state.analysisBusy;
    const visible = outcome && state.view === 'cleanup' && !busy && !$('cleanup-dialog').open;
    panel.classList.toggle('hidden', !visible);
    if (!visible) return;
    const hasRecords = state.receipts.length > 0;
    const canOpenTrash = desktop && !demo && (outcome.unknown || outcome.movedCount > 0 || hasRecords);
    panel.className = `cleanup-outcome ${outcome.type}`;
    panel.innerHTML = `<div class="cleanup-outcome-copy">${icon(outcome.type === 'success' ? 'check-circle' : outcome.type === 'error' ? 'alert' : 'info')}<strong title="${escape(outcome.detail)}">${escape(outcome.summary)}</strong></div><div class="cleanup-outcome-actions"><button class="text-button" data-cleanup-open-trash ${canOpenTrash ? '' : 'disabled'}>打开废纸篓</button><button class="text-button" data-cleanup-view-records ${hasRecords ? '' : 'disabled'}>查看记录</button><button class="cleanup-outcome-dismiss" data-cleanup-dismiss aria-label="关闭清理结果">×</button></div>`;
  }
  function viewCleanupRecords() {
    if (state.view !== 'cleanup' || state.scanning || state.cleaning || state.analysisBusy || !state.receipts.length) return;
    renderReceipts();
    const panel = $('receipt-panel');
    panel.open = true;
    panel.scrollIntoView({ block: 'nearest', behavior: 'smooth' });
    panel.querySelector('summary')?.focus({ preventScroll: true });
  }
  function renderWarnings() {
    const warnings = [...(state.report?.warnings || []), ...state.extraWarnings];
    const panel = $('warning-panel');
    panel.classList.toggle('hidden', warnings.length === 0 && !state.report?.cancelled);
    const incomplete = state.report?.cancelled || (state.report?.warnings || []).some((warning) => /部分结果|上限|无法.*读取|占用未知|已跳过|不完整|已停用/.test(warning));
    $('warning-title').textContent = state.report?.cancelled ? '已停止 · 部分结果' : `${incomplete ? '部分结果 · ' : ''}${warnings.length} 条提醒`;
    $('warning-summary').textContent = state.report?.cancelled ? '没有检查完所有位置；已发现的内容可以继续查看。' : `${warnings.length} 条提示，可点开查看。无法读取的内容不会被清理。`;
    $('warning-list').innerHTML = warnings.map((warning) => `<li>${escape(warning)}</li>`).join('');

  }
  function renderReceipts() {
    $('receipt-panel').classList.toggle('hidden', state.receipts.length === 0);
    $('receipt-summary').textContent = `已移动 ${state.receipts.length} 项 · 查看记录与原位置`;
    $('receipt-list').innerHTML = state.receipts.map((item) => `<li><div><strong>${escape(item.name)}</strong><span>${bytes(item.bytes)} · ${escape(item.movedAt)}</span></div><code>${escape(item.path)}</code></li>`).join('');
    renderReceiptAction();
  }
  function renderReceiptAction() {
    const button = $('show-receipts');
    if (!button) return;
    button.classList.toggle('hidden', state.receipts.length === 0 || Boolean(state.cleanOutcome));
    button.disabled = state.scanning || state.cleaning || state.analysisBusy;
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
  function restoreResultFocus(attributes) {
    const controls = $('result-rows').querySelectorAll('[data-item-id], [data-group-id], [data-tree-id], [data-tree-toggle], [data-file-info], [data-tree-page]');
    const control = [...controls].find((element) => Object.entries(attributes).every(([key, value]) => element.dataset[key] === value));
    if (control && !control.disabled) { control.focus({ preventScroll: true }); return true; }
    return false;
  }
  function renderCandidate(item, busy, depth) {
    const failure = state.failures.get(item.id);
    const infoOpen = state.expandedFileInfo.has(item.id);
    return `<div class="tree-file${failure ? ' failed' : ''}" data-tree-depth="${Math.min(depth, 5)}"><div class="tree-file-row"><input type="checkbox" data-item-id="${escape(item.id)}" ${state.selected.has(item.id) ? 'checked' : ''} ${busy ? 'disabled' : ''} aria-label="选择 ${escape(item.name)}" /><span class="tree-file-icon">${icon('file')}</span><strong class="tree-file-name" title="${escape(item.name)}">${escape(item.name)}</strong>${failure ? '<span class="tree-failure-count">未能移动</span>' : ''}<span class="tree-file-size">${bytes(item.bytes)}</span><button class="tree-file-info-button" data-file-info="${escape(item.id)}" aria-expanded="${infoOpen}" aria-label="${infoOpen ? '收起' : '查看'} ${escape(item.name)} 的${failure ? '失败原因与' : ''}文件信息" title="${failure ? '查看失败原因与文件信息' : '查看文件信息'}" ${busy ? 'disabled' : ''}>${icon('info')}</button><button class="row-reveal" data-reveal-id="${escape(item.id)}" title="在 Finder 中显示此项目" aria-label="在 Finder 中显示 ${escape(item.name)}" ${!desktop || demo || busy ? 'disabled' : ''}>${icon('folder')}</button></div>${infoOpen ? `<div class="tree-file-info">${failure ? `<p class="tree-file-error">未能移动：${escape(failure)}</p>` : ''}<p>${escape(item.reason)}</p><code>${escape(item.path)}</code><p class="tree-file-meta">修改于 ${date(item.modifiedAt)} · ${Number(item.files || 0).toLocaleString()} 个文件</p></div>` : ''}</div>`;
  }
  function renderTreeFiles(node, key, busy, depth) {
    if (!node.files.length) return '';
    const page = cleanupModel.pageItems(node.files, state.treePages.get(key) || 1, PAGE_SIZE);
    state.treePages.set(key, page.page);
    const heading = node.children.length ? `<div class="tree-file-section" data-tree-depth="${Math.min(depth, 5)}">此目录内容 · ${node.files.length.toLocaleString()} 项</div>` : '';
    return heading + page.items.map((item) => renderCandidate(item, busy, depth)).join('') + (page.pages > 1 ? `<div class="tree-file-pagination" data-tree-depth="${Math.min(depth, 5)}"><span>此目录项目 ${page.first}–${page.last} / ${node.files.length.toLocaleString()}</span><button class="page-button" data-tree-page="previous" data-tree-key="${escape(key)}" ${busy || page.page <= 1 ? 'disabled' : ''}>上一页</button><span>${page.page} / ${page.pages}</span><button class="page-button" data-tree-page="next" data-tree-key="${escape(key)}" ${busy || page.page >= page.pages ? 'disabled' : ''}>下一页</button></div>` : '');
  }
  function renderTreeDirectory(node, group, busy, depth = 0) {
    const key = JSON.stringify([group.key, node.key]);
    state.treeNodes.set(key, { node, groupKey: group.key });
    const selected = cleanupModel.selectedState(node, state.selected);
    const open = state.expandedTree.has(key);
    const failedCount = node.items.filter((item) => state.failures.has(item.id)).length;
    return `<div class="tree-directory" data-tree-depth="${Math.min(depth, 5)}"><div class="tree-directory-row${selected.count ? ' selected' : ''}"><input type="checkbox" data-tree-id="${escape(key)}" ${selected.checked ? 'checked' : ''} ${busy ? 'disabled' : ''} aria-label="选择 ${escape(node.name)} 下本次找到的全部 ${node.items.length} 项内容" /><button class="tree-directory-toggle" data-tree-toggle="${escape(key)}" aria-expanded="${open}" title="${escape(node.path)}" aria-label="${open ? '收起' : '查看'} ${escape(node.name)} 下的待检查内容" ${busy ? 'disabled' : ''}><span class="tree-directory-icon">${icon('folder')}</span><span class="tree-directory-name"><strong>${escape(node.name)}</strong><small>${node.items.length.toLocaleString()} 项待检查${failedCount ? ` · <span class="tree-failure-count">${failedCount} 项未能移动</span>` : ''}</small></span><span class="tree-directory-size">${bytes(node.bytes)}</span><span class="tree-directory-chevron">${icon('chevron-down')}</span></button></div>${open ? node.children.map((child) => renderTreeDirectory(child, group, busy, depth + 1)).join('') + renderTreeFiles(node, key, busy, depth + 1) : ''}</div>`;
  }
  function renderGroups(groups, busy, filtered) {
    state.treeNodes.clear();
    $('result-rows').innerHTML = groups.map((group) => {
      const selected = cleanupModel.selectedState(group, state.selected);
      const failures = group.items.filter((item) => state.failures.has(item.id));
      const open = state.expanded.has(group.key);
      const count = `${filtered ? '当前筛选 · ' : ''}${group.items.length.toLocaleString()} 项`;
      const tree = open ? cleanupModel.buildDirectoryTree(group.items, state.sort) : [];
      const directories = open ? `<div class="group-tree"><p class="group-tree-note">${escape(explanations[group.category] || '')} 目录大小只统计本次找到的内容，未列出的文件不会被清理。</p>${tree.map((node) => renderTreeDirectory(node, group, busy)).join('')}</div>` : '';
      return `<article class="cleanup-group${selected.count ? ' selected' : ''}${failures.length ? ' failed' : ''}"><div class="group-row"><input type="checkbox" data-group-id="${escape(group.key)}" ${selected.checked ? 'checked' : ''} ${busy || (!group.suggested && !open) ? 'disabled' : ''} aria-label="选择 ${escape(group.name)} 的全部 ${group.items.length} 个匹配项目" /><span class="group-icon ${escape(group.category)}">${icon(categoryIcons[group.category])}</span><div class="group-name"><strong>${escape(group.name)}</strong><span>${escape(labels[group.category] || group.category)} · ${count}</span></div>${!group.suggested ? '<span class="group-badge review">需确认</span>' : ''}<strong class="group-size">${bytes(group.bytes)}</strong></div>${failures.length ? `<p class="group-error">${failures.length} 项未能移动，展开查看原因</p>` : ''}<details class="group-details" data-group-details="${escape(group.key)}" ${open ? 'open' : ''}><summary title="${open ? '收起' : '查看'} ${escape(group.name)} 的目录" aria-label="${open ? '收起' : '查看'} ${escape(group.name)} 的目录" aria-disabled="${busy}"><span class="sr-only">查看目录</span><span class="group-chevron">${icon('chevron-down')}</span></summary>${directories}</details></article>`;
    }).join('');
    $('result-rows').querySelectorAll('[data-tree-depth]').forEach((element) => {
      element.style.setProperty('--tree-depth', Number(element.dataset.treeDepth));
    });
    $('result-rows').querySelectorAll('[data-group-id]').forEach((checkbox) => {
      checkbox.indeterminate = cleanupModel.selectedState(state.groups.get(checkbox.dataset.groupId), state.selected).indeterminate;
    });
    $('result-rows').querySelectorAll('[data-tree-id]').forEach((checkbox) => {
      const entry = state.treeNodes.get(checkbox.dataset.treeId);
      checkbox.indeterminate = cleanupModel.selectedState(entry.node, state.selected).indeterminate;
    });
  }
  function render() {
    const items = activeItems();
    const selected = selectedItems();
    const visible = visibleItems();
    const allGroups = cleanupModel.groupItems(visible, state.sort);
    state.groups = new Map(allGroups.map((group) => [group.key, group]));
    const groupPage = cleanupModel.pageItems(allGroups, state.page, GROUP_PAGE_SIZE);
    state.page = groupPage.page;
    const pages = groupPage.pages;
    const pageItems = groupPage.items.flatMap((group) => group.items);
    const busy = state.scanning || state.cleaning || state.analysisBusy;
    const titles = { home: '我的 Mac', cleanup: '建议清理', analysis: '空间去哪里了' };
    $('main-title').textContent = titles[state.view];
    if (state.view !== 'analysis') $('page-label').textContent = state.view === 'home' ? '先检查，再由你决定清理什么' : '';
    ['home', 'cleanup', 'analysis'].forEach((view) => {
      $(`show-${view}`).classList.toggle('active', state.view === view);
      $(`show-${view}`).disabled = state.cleaning || (view !== state.view && (state.scanning || state.analysisBusy));
    });
    $('toggle-settings').classList.toggle('hidden', state.view !== 'cleanup');
    $('start-scan').classList.toggle('hidden', state.view === 'home');
    $('start-scan').classList.toggle('primary', state.view === 'analysis');
    $('start-scan').classList.toggle('secondary', state.view !== 'analysis');
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
    $('scan-summary-title').textContent = state.report ? `${allGroups.length} 组内容${state.report.cancelled ? ' · 检查已停止' : ''}` : '先开始一次检查';
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
    renderGroups(groupPage.items, busy, Boolean(state.search || state.filter !== 'all' || state.risk !== 'all'));
    const all = $('select-all');
    const pageSuggested = pageItems.filter((item) => item.selectedByDefault === true);
    all.checked = pageSuggested.length > 0 && pageSuggested.every((item) => state.selected.has(item.id));
    all.indeterminate = !all.checked && pageSuggested.some((item) => state.selected.has(item.id));
    all.disabled = busy || pageSuggested.length === 0;
    all.setAttribute('aria-label', `选择当前列表中的 ${pageSuggested.length} 个建议项`);
    $('select-safe').disabled = busy || !items.some((item) => item.selectedByDefault === true);
    $('clear-selection').disabled = busy || selected.length === 0;
    $('selection-count').textContent = selected.length ? `已选 ${bytes(sum(selected))}` : '请选择要清理的内容';
    const reviewCount = selected.filter((item) => item.risk !== 'low').length;
    $('selection-description').textContent = selected.length ? (reviewCount ? `包含 ${reviewCount} 项需要你确认的内容` : '移到废纸篓前，会再次让你确认。') : '移到废纸篓前，会再次让你确认。';
    $('review-cleanup').disabled = busy || selected.length === 0 || !desktop || demo;
    $('review-cleanup').title = demo ? '演示模式禁止清理' : !desktop ? '请在桌面应用中使用' : '';
    if (state.view !== 'analysis') {
      $('start-scan').disabled = busy || !desktop || demo;
      $('scan-button-text').textContent = state.scanning ? '检查中…' : state.report ? '重新检查' : '开始检查';
    }
    $('app-status').textContent = demo ? '演示模式' : state.cleaning ? '正在清理' : state.analysisBusy ? '正在查看文件夹大小' : state.scanning ? '正在检查' : desktop ? '准备就绪' : '请打开桌面应用';
    document.querySelector('.status-dot').classList.toggle('busy', busy);
    $('open-trash').disabled = !desktop || demo;
    $('cancel-scan').disabled = state.cancelling;
    $('cancel-scan').textContent = state.cancelling ? '正在停止…' : '停止';
    $('progress-panel').classList.toggle('hidden', !state.scanning);
    const range = allGroups.length ? `${groupPage.first}–${groupPage.last}` : '0';
    $('results-footer-detail').textContent = pages > 1 ? `分组 ${range} / ${allGroups.length}` : '';
    $('pagination').classList.toggle('hidden', pages <= 1);
    $('cleanup-group-pages').classList.toggle('hidden', pages <= 1);
    $('cleanup-view').classList.toggle('has-group-pages', pages > 1);
    $('page-label-counter').textContent = `${state.page} / ${pages}`;
    $('previous-page').disabled = state.page <= 1;
    $('next-page').disabled = state.page >= pages;
    $('scan-settings').querySelectorAll('input, select').forEach((el) => { el.disabled = busy; });
    renderDisk();
    renderHome();
    renderWarnings();
    renderReceipts();
    renderCleanOutcome();
    renderMessage();
    window.dispatchEvent(new CustomEvent('mac-sweep-state'));
  }
  function applyReport(report) {
    if (!report || !Array.isArray(report.items) || typeof report.scanId !== 'string') throw new Error('扫描结果格式无效，已保留之前的结果。');
    state.report = report;
    state.selected = new Set(report.items.filter((item) => item.selectedByDefault === true).map((item) => item.id));
    state.moved.clear();
    state.failures.clear();
    state.expanded.clear();
    state.expandedTree.clear();
    state.expandedFileInfo.clear();
    state.treePages.clear();
    state.treeNodes.clear();
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
    state.cleanOutcome = null;
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
    state.cleanOutcome = null;
    renderCleanOutcome();
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
  function validCleanupReport(report, request) {
    if (!report || !Array.isArray(report.moved) || !Array.isArray(report.failed) || !Number.isFinite(report.bytesMoved) || report.bytesMoved < 0) return false;
    const requested = new Set(request.itemIds);
    const seen = new Set();
    for (const item of [...report.moved, ...report.failed]) {
      if (!item || typeof item.id !== 'string' || !requested.has(item.id) || seen.has(item.id) || typeof item.path !== 'string' || !item.path || !Number.isFinite(item.bytes) || item.bytes < 0) return false;
      seen.add(item.id);
    }
    return seen.size === requested.size && (report.moved.length > 0 || report.bytesMoved === 0);
  }
  async function cleanup() {
    const request = state.dialogSelection;
    if (!desktop || demo || !request || state.cleaning || state.scanning || request.scanId !== state.report?.scanId) return;
    state.cleaning = true;
    state.cleanOutcome = null;
    $('dialog-cancel').disabled = true;
    $('dialog-confirm').disabled = true;
    $('dialog-confirm').textContent = '正在移动…';
    $('dialog-progress').classList.remove('hidden');
    $('cleanup-progress-bar').style.width = '0%';
    $('cleanup-progress-text').textContent = '正在准备移到废纸篓…';
    render();
    try {
      const report = await invoke('clean_items', request);
      if (!validCleanupReport(report, request)) throw new Error('清理回执不完整或格式无效。请在 Finder 检查文件位置后重新扫描。');
      const movedAt = new Date().toLocaleString('zh-CN', { hour12: false });
      const sourceById = new Map((state.report?.items || []).map((item) => [item.id, item]));
      report.moved.forEach((item) => { state.receipts.unshift({ path: item.path, name: sourceById.get(item.id)?.name || String(item.path).split('/').pop(), bytes: item.bytes, movedAt }); });
      report.moved.forEach((item) => { state.moved.add(item.id); state.selected.delete(item.id); state.failures.delete(item.id); });
      report.failed.forEach((item) => { state.failures.set(item.id, item.error || '移动失败'); });
      const failed = report.failed.length;
      const moved = report.moved.length;
      state.cleanOutcome = {
        type: failed ? 'info' : 'success', movedCount: moved, bytesMoved: report.bytesMoved, failedCount: failed, unknown: false,
        summary: `${moved ? `已移到废纸篓 ${moved} 项 · ${bytes(report.bytesMoved)}` : ''}${failed ? `${moved ? ' · ' : ''}${failed} 项未完成，选择已保留` : ''}`,
        detail: failed ? '未完成的项目仍保留选择，展开对应文件可查看原因。确认废纸篓中的文件不再需要后，再自行清空。' : '文件可在 Finder 废纸篓中找回。确认不再需要后，再自行清空；移入废纸篓不会立即释放空间。',
      };
    } catch (error) {
      state.cleanOutcome = { type: 'error', unknown: true, summary: '清理结果未确认，请检查文件状态', detail: `${errorText(error)} 请检查 Finder 中的原文件与废纸篓，必要时重新扫描。` };
    }
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
    $('clear-receipts').addEventListener('click', () => { state.receipts = []; state.cleanOutcome = null; $('receipt-panel').open = false; renderReceipts(); renderCleanOutcome(); });
    $('show-receipts')?.addEventListener('click', viewCleanupRecords);
    $('show-analysis').addEventListener('click', () => setView('analysis'));
    document.querySelector('.brand').addEventListener('click', (event) => { event.preventDefault(); setView('home'); });
    document.querySelectorAll('[data-category]').forEach((button) => button.addEventListener('click', () => { if (!setView('cleanup')) return; state.filter = button.dataset.category; state.page = 1; render(); }));
    $('toggle-settings').addEventListener('click', () => { const open = $('scan-settings').classList.toggle('hidden') === false; $('toggle-settings').setAttribute('aria-expanded', String(open)); });
    document.addEventListener('pointerdown', (event) => {
      ['advanced-filters', 'warning-panel'].forEach((id) => { const panel = $(id); if (panel.open && !panel.contains(event.target)) panel.open = false; });
    });
    document.addEventListener('keydown', (event) => {
      if (event.key !== 'Escape' || $('cleanup-dialog').open) return;
      const panel = ['advanced-filters', 'warning-panel'].map($).find((element) => element.open);
      if (panel) { event.preventDefault(); panel.open = false; panel.querySelector('summary')?.focus(); }
    });
    document.addEventListener('pointerdown', (event) => { if (!$('scan-settings').classList.contains('hidden') && !$('scan-settings').contains(event.target) && !$('toggle-settings').contains(event.target)) closeSettings(); });
    document.addEventListener('keydown', (event) => { if (event.key === 'Escape' && !$('scan-settings').classList.contains('hidden')) { event.preventDefault(); closeSettings(); $('toggle-settings').focus(); } });
    $('search').addEventListener('input', (event) => { state.search = event.target.value; state.page = 1; render(); });
    $('risk-filter').addEventListener('change', (event) => { state.risk = event.target.value; state.page = 1; render(); });
    $('sort').addEventListener('change', (event) => { state.sort = event.target.value; state.page = 1; render(); });
    $('previous-page').addEventListener('click', () => { if (state.page <= 1) return; state.page -= 1; render(); $('result-table').scrollIntoView({ block: 'start' }); });
    $('next-page').addEventListener('click', () => { if (state.page >= Math.ceil(cleanupModel.groupItems(visibleItems(), state.sort).length / GROUP_PAGE_SIZE)) return; state.page += 1; render(); $('result-table').scrollIntoView({ block: 'start' }); });
    $('result-rows').addEventListener('change', (event) => {
      if (state.scanning || state.cleaning || state.analysisBusy) return;
      const checkbox = event.target.closest('[data-item-id], [data-group-id], [data-tree-id]');
      if (!checkbox || checkbox.disabled) return;
      const tree = checkbox.dataset.treeId ? state.treeNodes.get(checkbox.dataset.treeId) : null;
      const group = tree ? state.groups.get(tree.groupKey) : checkbox.dataset.groupId ? state.groups.get(checkbox.dataset.groupId) : null;
      if ((checkbox.dataset.treeId && !tree) || (checkbox.dataset.groupId && !group)) return;
      if (group && !group.suggested && !state.expanded.has(group.key)) return;
      const targets = tree ? tree.node.items : group ? group.items : visibleItems().filter((item) => item.id === checkbox.dataset.itemId);
      targets.forEach((item) => { if (checkbox.checked) state.selected.add(item.id); else state.selected.delete(item.id); });
      const attribute = tree ? 'treeId' : group ? 'groupId' : 'itemId';
      const id = checkbox.dataset[attribute];
      render();
      restoreResultFocus({ [attribute]: id });
    });
    $('result-rows').addEventListener('toggle', (event) => {
      const details = event.target;
      if (!details.matches('[data-group-details]')) return;
      const key = details.dataset.groupDetails;
      if (!state.groups.has(key) || details.open === state.expanded.has(key)) return;
      if (state.scanning || state.cleaning || state.analysisBusy) { details.open = state.expanded.has(key); return; }
      if (details.open) state.expanded.add(key); else state.expanded.delete(key);
      render();
      const replacement = [...$('result-rows').querySelectorAll('[data-group-details]')].find((el) => el.dataset.groupDetails === key);
      replacement?.querySelector('summary')?.focus({ preventScroll: true });
    }, true);
    $('result-rows').addEventListener('click', (event) => {
      if (state.scanning || state.cleaning || state.analysisBusy) {
        if (event.target.closest('[data-group-details] > summary')) event.preventDefault();
        return;
      }
      const treeButton = event.target.closest('[data-tree-toggle]');
      if (treeButton) {
        const key = treeButton.dataset.treeToggle;
        if (!state.treeNodes.has(key) || treeButton.disabled) return;
        if (state.expandedTree.has(key)) state.expandedTree.delete(key); else state.expandedTree.add(key);
        render();
        restoreResultFocus({ treeToggle: key });
        return;
      }
      const infoButton = event.target.closest('[data-file-info]');
      if (infoButton) {
        const id = infoButton.dataset.fileInfo;
        if (infoButton.disabled || !visibleItems().some((item) => item.id === id)) return;
        if (state.expandedFileInfo.has(id)) state.expandedFileInfo.delete(id); else state.expandedFileInfo.add(id);
        render();
        restoreResultFocus({ fileInfo: id });
        return;
      }
      const pageButton = event.target.closest('[data-tree-page]');
      if (pageButton) {
        if (pageButton.disabled) return;
        const key = pageButton.dataset.treeKey;
        const entry = state.treeNodes.get(key);
        if (!entry) return;
        const page = cleanupModel.pageItems(entry.node.files, state.treePages.get(key) || 1, PAGE_SIZE);
        state.treePages.set(key, page.page + (pageButton.dataset.treePage === 'next' ? 1 : -1));
        render();
        if (!restoreResultFocus({ treePage: pageButton.dataset.treePage, treeKey: key })) restoreResultFocus({ treePage: pageButton.dataset.treePage === 'next' ? 'previous' : 'next', treeKey: key });
        return;
      }
      const button = event.target.closest('[data-reveal-id]');
      if (button && !button.disabled) revealItem(button.dataset.revealId);
    });
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
    $('cleanup-outcome')?.addEventListener('click', (event) => {
      const button = event.target.closest('[data-cleanup-open-trash], [data-cleanup-view-records], [data-cleanup-dismiss]');
      if (!button || button.disabled || state.view !== 'cleanup' || state.scanning || state.cleaning || state.analysisBusy) return;
      if (button.hasAttribute('data-cleanup-open-trash')) openTrash();
      else if (button.hasAttribute('data-cleanup-view-records')) viewCleanupRecords();
      else {
        state.cleanOutcome = null;
        renderCleanOutcome();
        const focus = !$('review-cleanup').disabled ? $('review-cleanup') : !$('start-scan').disabled ? $('start-scan') : $('show-cleanup');
        focus.focus({ preventScroll: true });
      }
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
