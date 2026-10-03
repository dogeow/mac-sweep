(() => {
  'use strict';
  const app = window.macSweep;
  const navigation = window.macSweepAnalysisNavigation;
  if (!app || !navigation) return;
  const $ = (id) => document.getElementById(id);
  const escape = app.escapeHtml;
  const icon = app.icon;
  const invoke = (command, args) => window.__TAURI__.core.invoke(command, args);
  const state = { locations: [], path: '', history: [], revision: 0, scanning: false, browsing: false, recovering: false, cancelling: false, page: 1, sort: 'size-desc', chartNodes: new Map(), browseCache: new Map(), invalidatedNodes: new Map(), staleDirectories: new Map(), emptyReason: null, message: null, showChart: false, showDetails: false, scanPath: '', progress: null, scanStartedAt: 0, lastProgressAt: 0, progressTimer: null };
  const PAGE_SIZE = 100;
  const palette = ['#5b94ce', '#8b86c6', '#6aaa97', '#bf9167', '#b17eaa', '#829bb5', '#a39a6a', '#7188b9', '#ba8d85', '#8eaa78', '#8398a2', '#a29bb8'];
  const formatBytes = (value) => {
    const number = Math.max(0, Number(value) || 0);
    if (number < 1000) return `${number.toLocaleString('zh-CN')} B`;
    const units = ['B', 'KB', 'MB', 'GB', 'TB'];
    const index = Math.min(Math.floor(Math.log(number) / Math.log(1000)), units.length - 1);
    return `${(number / 1000 ** index).toLocaleString('zh-CN', { maximumFractionDigits: 1 })} ${units[index]}`;
  };
  const sizeKnown = (node) => node?.sizeKnown !== false;
  const sizeLabel = (node) => !sizeKnown(node) ? '待分析' : node.partial && !node.bytes ? '大小未知' : formatBytes(node.bytes);
  function measuredTime(report) {
    const timestamp = report?.measuredAt;
    if (typeof timestamp !== 'number' || !Number.isFinite(timestamp) || timestamp <= 0) return '';
    const date = new Date(timestamp);
    if (!Number.isFinite(date.getTime())) return '';
    return ` · 统计于 ${date.toLocaleString('zh-CN', { month: '2-digit', day: '2-digit', hour: '2-digit', minute: '2-digit', hour12: false })}`;
  }
  function storageAccounting(node, report) {
    const storage = report?.storage;
    if (!node || !storage || storage.isVolumeRoot !== true || node.path !== report.root?.path) return null;
    const finiteSize = (value) => typeof value === 'number' && Number.isFinite(value) && value >= 0;
    if (![storage.totalBytes, storage.availableBytes, storage.usedBytes].every(finiteSize)) return null;
    const located = Math.max(0, Number(node.bytes) || 0);
    const otherVolumesKnown = finiteSize(storage.otherVolumesBytes);
    const otherVolumes = otherVolumesKnown ? storage.otherVolumesBytes : 0;
    const volumeUsed = finiteSize(storage.volumeUsedBytes) ? storage.volumeUsedBytes : Math.max(0, storage.usedBytes - otherVolumes);
    const differentMeasurement = located > volumeUsed || located + otherVolumes > storage.usedBytes;
    return { storage, located, otherVolumes, otherVolumesKnown, volumeUsed, differentMeasurement, unlocated: differentMeasurement ? null : Math.max(0, storage.usedBytes - located - otherVolumes) };
  }
  function renderStorageDetails(accounting) {
    const coverage = current()?.report;
    const otherErrors = Math.max(0, Number(coverage?.otherErrorCount || 0) - Number(coverage?.blockedDirectoryCount || 0));
    const issues = [Number(coverage?.permissionDeniedCount || 0) ? `无权读取 ${Number(coverage.permissionDeniedCount).toLocaleString()} 处` : '', otherErrors ? `其他读取失败 ${otherErrors.toLocaleString()} 处` : '', Number(coverage?.skippedMountCount || 0) ? `跳过其他挂载 ${Number(coverage.skippedMountCount).toLocaleString()} 处` : '', Number(coverage?.changedDirectoryCount || 0) ? `扫描期间变化 ${Number(coverage.changedDirectoryCount).toLocaleString()} 处` : '', Number(coverage?.depthLimitedCount || 0) ? `未继续展开 ${Number(coverage.depthLimitedCount).toLocaleString()} 处` : '', Number(coverage?.cloudPlaceholderCount || 0) ? `${Number(coverage.cloudPlaceholderCount).toLocaleString()} 处云盘在线占位（未下载）` : '', Number(coverage?.blockedDirectoryCount || 0) ? `受保护目录未读取 ${Number(coverage.blockedDirectoryCount).toLocaleString()} 处` : ''].filter(Boolean);
    $('analysis-storage-info').classList.toggle('hidden', !accounting && !issues.length);
    $('analysis-storage-info').querySelector('summary').textContent = accounting ? '空间统计说明' : '扫描说明';
    if (!accounting) { $('analysis-space-details').innerHTML = issues.length ? `<p>本次分析：${escape(issues.join(' · '))}</p><p>只统计能够读取的本机文件块。${Number(coverage?.cloudPlaceholderCount || 0) ? '云盘在线内容未下载，不按云端文件大小计入本机占用。' : ''}</p>` : ''; return; }
    const { storage, located, otherVolumes, otherVolumesKnown, unlocated, differentMeasurement } = accounting;
    const rows = [['磁盘总容量', storage.totalBytes], ['系统已用', storage.usedBytes], ['系统可用', storage.availableBytes], ['已定位文件', located]];
    if (otherVolumes > 0) rows.push(['其他 APFS 卷', otherVolumes]);
    if (unlocated !== null) rows.push(['未定位', unlocated]);
    const isApfs = String(storage.filesystem || '').toLowerCase() === 'apfs';
    const unknownVolumes = isApfs ? otherVolumesKnown ? '其他 APFS 卷不属于当前文件夹。' : '其他 APFS 卷的占用尚未细分，也可能包含在未定位空间内。' : '';
    $('analysis-space-details').innerHTML = `<dl>${rows.map(([label, value]) => `<dt>${escape(label)}</dt><dd>${formatBytes(value)}</dd>`).join('')}</dl><p>系统已用来自 macOS 的空间计量；已定位文件来自本次能读取的文件块统计。</p>${differentMeasurement ? '<p class="storage-accounting-warning">文件块统计与系统计量不同，可能包含共享块重复计量。两者不能直接相减。</p>' : `<p>未定位空间可能包括无权读取的文件、快照和文件系统开销，不能直接清理。${unknownVolumes}</p>`}${issues.length ? `<p>本次分析：${escape(issues.join(' · '))}</p>` : ''}<p>${escape(storage.filesystem || '文件系统')} · ${escape(storage.source || 'macOS 系统空间计量')}</p>`;
  }
  function accountingRows(accounting) {
    if (!accounting) return '';
    const rows = [];
    if (accounting.otherVolumes > 0) rows.push(['其他 APFS 卷', accounting.otherVolumes, '当前卷之外的系统占用']);
    if (accounting.unlocated > 0) rows.push(['未归到文件夹的空间', accounting.unlocated, '查看统计说明，不能直接清理']);
    return rows.map(([label, size, detail]) => `<tr class="analysis-accounting-row"><td><button class="analysis-accounting-label" data-analysis-accounting aria-label="${escape(label)}，查看统计说明">${icon('info')}<span>${escape(label)}<small>${escape(detail)}</small></span></button></td><td class="analysis-size">${formatBytes(size)}</td><td class="analysis-accounting-kind">系统计量</td><td></td><td></td></tr>`).join('');
  }
  function friendlyName(node) {
    const location = state.locations.find((entry) => entry.path === node?.path);
    if (location) return location.label;
    const home = state.locations.find((entry) => entry.id === 'home')?.path;
    const common = { Library: '应用与系统文件', Documents: '文稿', Pictures: '图片', Movies: '视频', Music: '音乐', Desktop: '桌面', Applications: '应用程序', '.Trash': '废纸篓', '.cache': '工具保存的文件' };
    if (home && node?.path === `${home}/${node.name}` && common[node.name]) return common[node.name];
    return node?.name || node?.path || '文件夹';
  }
  function friendlyPath(path) {
    return friendlyName({ path, name: path?.split('/').filter(Boolean).pop() || '文件夹' });
  }
  const current = () => navigation.current(state.history);
  function alignBreadcrumbs() {
    const breadcrumbs = $('analysis-breadcrumbs');
    breadcrumbs.scrollLeft = breadcrumbs.scrollWidth;
  }
  const errorText = (error) => typeof error === 'string' ? error : error?.message || '操作未能完成。';
  const originOf = (report) => report.sourceAnalysisId || report.analysisId;
  const containsPath = (parent, path) => parent === path || Boolean(parent && path && path.startsWith(parent === '/' ? '/' : `${parent}/`));
  function invalidStatus(error) {
    const text = errorText(error);
    if (text.startsWith('ANALYSIS_NODE_MISSING:')) return 'missing';
    if (text.startsWith('ANALYSIS_NODE_CHANGED:')) return 'changed';
    return null;
  }
  function friendlyFailure(error, action, preserve = false) {
    const text = errorText(error);
    const detail = /过期|expired/i.test(text) ? '这份分析结果已过期，请重新分析。' : /权限|permission denied|operation not permitted|access denied/i.test(text) ? `暂时无法${action}，请检查访问权限后重试。` : `暂时无法${action}，请稍后重试。`;
    return detail + (preserve ? '已保留当前结果。' : '');
  }
  function invalidateNode(target, sourceReport) {
    const origin = originOf(sourceReport);
    const invalid = state.invalidatedNodes.get(origin) || new Set();
    const stale = state.staleDirectories.get(origin) || new Set();
    invalid.add(target.id);
    state.invalidatedNodes.set(origin, invalid);
    state.staleDirectories.set(origin, stale);
    const visited = new Set();
    function prune(node) {
      if (!node || visited.has(node)) return;
      visited.add(node);
      const unavailable = node.id === target.id || node.path === target.path || (target.kind === 'directory' && containsPath(target.path, node.path));
      if (unavailable) {
        invalid.add(node.id);
        node.unavailable = true;
        node.stale = true;
      }
      if (node.kind === 'directory' && containsPath(node.path, target.path)) {
        stale.add(node.path);
        node.stale = true;
      }
      for (const child of node.children || []) prune(child);
      node.children = (node.children || []).filter((child) => !child.unavailable);
      if (unavailable) { node.children = []; node.omittedChildren = 0; node.hasChildren = false; }
    }
    for (const entry of state.history) {
      if (originOf(entry.report) !== origin) continue;
      prune(entry.report.root);
      prune(entry.node);
      entry.stale = Boolean(entry.node.stale);
      entry.invalid = Boolean(entry.node.unavailable);
    }
    for (const [key, report] of state.browseCache) {
      if (originOf(report) !== origin) continue;
      prune(report.root);
      state.browseCache.delete(key);
    }
  }
  function filterInvalidNodes(report) {
    const origin = originOf(report);
    const invalid = state.invalidatedNodes.get(origin);
    const stale = state.staleDirectories.get(origin);
    const pending = [report.root];
    while (pending.length) {
      const node = pending.pop();
      node.children = (node.children || []).filter((child) => !invalid?.has(child.id));
      if (sizeKnown(node) && stale?.has(node.path)) node.stale = true;
      pending.push(...node.children);
    }
    return report;
  }
  function listingFailed(report) {
    const errors = Number(report.otherErrorCount) || 0;
    const blocked = Number(report.blockedDirectoryCount) || 0;
    // Timed-out workers may return usable cached directory rows. Keep that
    // fallback, while refusing to replace an old list with permission/IO failure.
    return report.root?.partial && (Number(report.permissionDeniedCount) > 0 || errors > blocked);
  }
  function closeOptions(restoreFocus = false) {
    $('analysis-options').open = false;
    if (restoreFocus) $('analysis-options').querySelector('summary').focus();
  }
  function message(type, text) { state.message = { type, text }; renderMessage(); }
  function renderMessage() {
    $('analysis-message').className = `analysis-message${state.message ? ` ${state.message.type}` : ' hidden'}`;
    $('analysis-message').textContent = state.message?.text || '';
  }
  function updateLocations() {
    const entries = [...state.locations];
    if (state.path && !entries.some((entry) => entry.path === state.path)) entries.push({ id: 'custom', label: state.path.split('/').filter(Boolean).pop() || state.path, path: state.path });
    $('analysis-location').innerHTML = entries.length ? (!state.path ? '<option value="" selected>选择其他位置…</option>' : '') + entries.map((entry) => `<option value="${escape(entry.path)}" ${entry.path === state.path ? 'selected' : ''}>${escape(entry.label)}</option>`).join('') : '<option value="">请选择扫描位置</option>';
    $('analysis-selected-path').textContent = state.path || '选择文件夹以查看实际占用';
    $('analysis-selected-path').title = state.path;
  }
  function setPath(path) {
    if (state.scanning || !path) return;
    state.path = path;
    state.emptyReason = null;
    updateLocations();
    if (current() && current().node.path !== path) message('info', `已选择${friendlyPath(path)}。点击“开始分析”更新结果。`);
    else state.message = null;
    render();
  }
  function childrenFor(node) {
    const list = [...(node?.children || [])];
    return list.sort((a, b) => state.sort === 'name-asc' ? String(a.name).localeCompare(String(b.name), 'zh-CN') : Number(sizeKnown(b)) - Number(sizeKnown(a)) || b.bytes - a.bytes);
  }
  function sector(startAngle, endAngle, inner, outer) {
    if (endAngle - startAngle > 359.98) endAngle = startAngle + 359.98;
    const point = (radius, angle) => { const rad = (angle - 90) * Math.PI / 180; return [170 + radius * Math.cos(rad), 170 + radius * Math.sin(rad)]; };
    const outerStart = point(outer, startAngle), outerEnd = point(outer, endAngle);
    const innerEnd = point(inner, endAngle), innerStart = point(inner, startAngle);
    const large = endAngle - startAngle > 180 ? 1 : 0;
    return `M${outerStart.join(',')} A${outer},${outer} 0 ${large} 1 ${outerEnd.join(',')} L${innerEnd.join(',')} A${inner},${inner} 0 ${large} 0 ${innerStart.join(',')} Z`;
  }
  function renderChart(node) {
    state.chartNodes.clear();
    if (!node) return;
    if (node.stale) {
      $('analysis-chart').innerHTML = `<svg viewBox="0 0 340 340" aria-hidden="true"><circle cx="170" cy="170" r="107" fill="none" stroke="var(--disk-track)" stroke-width="70"/></svg><div class="chart-center"><strong>${sizeLabel(node)}</strong><span>已有统计</span><small>占用需更新</small></div>`;
      $('analysis-chart-key').innerHTML = '';
      $('analysis-chart-caption').textContent = '目录内容有变化，重新分析可更新占用。';
      return;
    }
    const busy = app.isBusy();
    const accounting = storageAccounting(node, current()?.report);
    const children = [...(node.children || [])].sort((a, b) => b.bytes - a.bytes);
    const hasUnknownChildren = children.some((child) => !sizeKnown(child));
    const displayed = children.filter((child) => sizeKnown(child) && child.bytes > 0).slice(0, 12);
    const measured = sizeKnown(node) ? Math.max(0, Number(node.bytes) || 0) : 0;
    const sum = displayed.reduce((total, child) => total + child.bytes, 0);
    const total = Math.max(measured, sum);
    let angle = 0;
    const paths = [];
    const legends = [];
    function draw(child, start, end, inner, outer, color, nested = false) {
      if (end - start < .08) return;
      state.chartNodes.set(child.id, child);
      const caption = `${friendlyName(child)} · ${formatBytes(child.bytes)}${child.partial ? ' · 未检查完整' : ''}`;
      paths.push(`<path d="${sector(start, end, inner, outer)}" fill="${color}" fill-opacity="${nested ? '.68' : '1'}" class="chart-segment" data-analysis-node="${escape(child.id)}" tabindex="${busy ? '-1' : '0'}" role="button" aria-disabled="${busy}" aria-label="${escape(caption)}"><title>${escape(caption)}</title></path>`);
    }
    for (const [index, child] of displayed.entries()) {
      const end = angle + (total ? child.bytes / total * 360 : 0);
      const color = palette[index % palette.length];
      draw(child, angle, end, 63, 111, color);
      const nested = [...(child.children || [])].filter((entry) => sizeKnown(entry) && entry.bytes > 0).sort((a, b) => b.bytes - a.bytes).slice(0, 7);
      let nestedAngle = angle;
      if (nested.length && child.bytes > 0) {
        for (const entry of nested) {
          const nestedEnd = Math.min(end, nestedAngle + (entry.bytes / child.bytes) * (end - angle));
          draw(entry, nestedAngle, nestedEnd, 113, 145, color, true);
          nestedAngle = nestedEnd;
        }
      }
      if (end - nestedAngle > .08) paths.push(`<path d="${sector(nestedAngle, end, 113, 145)}" fill="${color}" fill-opacity=".3" class="chart-aggregate"><title>${escape(child.name)} · 其他子项或未展开内容</title></path>`);
      legends.push(`<button data-analysis-node="${escape(child.id)}" title="${escape(child.name)}" ${busy ? 'disabled' : ''}><i class="chart-color-${index % palette.length}"></i><span>${escape(child.name)}</span><small>${formatBytes(child.bytes)}</small></button>`);
      angle = end;
    }
    const other = Math.max(0, total - sum);
    if (other > 0) {
      const otherLabel = hasUnknownChildren ? '尚未细分的已有统计' : '其他已读取项目';
      paths.push(`<path d="${sector(angle, 360, 63, 145)}" fill="var(--disk-track)" class="chart-aggregate"><title>${otherLabel} · ${formatBytes(other)}</title></path>`);
      legends.push(`<div><i class="other-key"></i><span>${otherLabel}</span><small>${formatBytes(other)}</small></div>`);
    }
    const circle = total > 0 ? paths.join('') : '<circle cx="170" cy="170" r="107" fill="none" stroke="var(--disk-track)" stroke-width="70"/>';
    $('analysis-chart').innerHTML = `<svg viewBox="0 0 340 340" role="group" aria-label="${escape(friendlyName(node))} 的大小图">${circle}</svg><div class="chart-center"><strong>${sizeLabel(node)}</strong><span>${escape(accounting ? '已定位文件' : friendlyName(node))}</span>${node.partial && sizeKnown(node) ? '<small>仅统计已读取</small>' : ''}</div>`;
    $('analysis-chart-key').innerHTML = legends.slice(0, 7).join('') + (legends.length > 7 ? `<span class="chart-more">另有 ${legends.length - 7} 项，可在右侧列表查看</span>` : '');
    $('analysis-chart-caption').textContent = !sizeKnown(node) ? '此目录尚无大小统计，可以选择完整分析。' : hasUnknownChildren ? '部分子目录尚无大小统计，可以选择完整分析。' : accounting ? accounting.differentMeasurement ? '文件夹块统计可能重复包含共享块，与系统计量不同。' : '已定位文件的分布，不含其他 APFS 卷和未定位空间。' : node.partial ? '部分文件未读取，大小未统计完整。' : '点击色块，查看里面的文件夹。';
  }
  function renderProgress() {
    const entry = current();
    $('analysis-progress').classList.toggle('hidden', (!state.scanning && !state.browsing) || !entry);
    $('analysis-progress-path').classList.toggle('hidden', state.browsing);
    $('analysis-progress-count').classList.toggle('hidden', state.browsing);
    if (state.browsing) {
      $('analysis-progress-label').textContent = state.cancelling ? '正在停止打开目录…' : `${state.recovering ? '正在更新列表' : '正在打开目录'}：${friendlyPath(state.scanPath)}`;
      $('analysis-progress-count').textContent = '';
      $('analysis-progress-path').textContent = '';
      return;
    }
    const progress = state.progress;
    const location = friendlyPath(state.scanPath || state.path);
    const path = progress?.currentPath || state.scanPath || state.path;
    const elapsed = Math.max(0, Math.floor((performance.now() - state.scanStartedAt) / 1000));
    const waiting = state.scanning && performance.now() - state.lastProgressAt >= 3000;
    const activity = state.cancelling ? '等待系统停止' : waiting ? '继续统计，等待系统返回' : '正在统计';
    const elapsedText = `已用 ${elapsed.toLocaleString()} 秒 · ${activity}`;
    const countText = `已查看 ${Number(progress?.scannedFiles || 0).toLocaleString()} 个文件 · 已统计 ${formatBytes(progress?.bytesFound)}`;
    const itemText = state.showDetails ? path : friendlyPath(path);
    const currentText = `${state.cancelling ? '最后读取' : '正在读取'}：${itemText}`;
    $('analysis-progress-label').textContent = `${state.cancelling ? '正在停止完整分析' : '正在完整分析，以下是上次结果'} · ${elapsedText}`;
    $('analysis-progress-path').textContent = currentText;
    $('analysis-progress-path').title = path;
    $('analysis-progress-count').textContent = countText;
    if (state.scanning && !entry) {
      const empty = $('analysis-empty');
      // Keep the same cancel button while progress or the elapsed timer updates.
      if (!empty.querySelector('[data-analysis-cancel]')) empty.innerHTML = '<span class="progress-spinner" aria-hidden="true"></span><strong class="analysis-empty-title"></strong><p class="analysis-empty-progress"></p><p class="analysis-empty-location"></p><p class="analysis-empty-time"></p><button class="text-button" data-analysis-cancel>停止分析</button>';
      empty.querySelector('.analysis-empty-title').textContent = state.cancelling ? '正在停止分析…' : `正在完整分析${location}`;
      empty.querySelector('.analysis-empty-progress').textContent = countText;
      empty.querySelector('.analysis-empty-location').textContent = currentText;
      empty.querySelector('.analysis-empty-location').title = path;
      empty.querySelector('.analysis-empty-time').textContent = elapsedText;
      const cancelButton = empty.querySelector('[data-analysis-cancel]');
      cancelButton.disabled = state.cancelling;
      cancelButton.textContent = state.cancelling ? '停止中…' : '停止分析';
    }
  }
  function stopProgressTimer() {
    if (state.progressTimer !== null) window.clearInterval(state.progressTimer);
    state.progressTimer = null;
  }
  function startProgressTimer(revision) {
    stopProgressTimer();
    state.scanStartedAt = performance.now();
    state.lastProgressAt = state.scanStartedAt;
    const timer = window.setInterval(() => {
      if (!state.scanning || revision !== state.revision) {
        window.clearInterval(timer);
        if (state.progressTimer === timer) state.progressTimer = null;
        return;
      }
      renderProgress();
    }, 1000);
    state.progressTimer = timer;
  }
  function render() {
    const entry = current();
    const node = entry?.node;
    const browsingSnapshot = entry?.browsed || entry?.report.cachedBrowse === true;
    // Navigation selects the displayed folder. An explicitly chosen location
    // controls the next scan until the user navigates to another folder.
    const selectedEntry = state.history.find((item) => item.node.path === state.path);
    const analysisAction = selectedEntry?.stale || selectedEntry?.node.stale ? '重新分析' : selectedEntry?.browsed || selectedEntry?.cached ? '完整分析' : selectedEntry ? '重新分析' : '开始分析';
    const accounting = storageAccounting(node, entry?.report);
    const hasAccountingRows = !!accounting && (accounting.otherVolumes > 0 || accounting.unlocated > 0);
    const busy = app.isBusy();
    $('analysis-view').classList.toggle('simple-mode', !state.showChart);
    $('analysis-view').classList.toggle('show-details', state.showDetails);
    $('analysis-view').classList.toggle('is-scanning', state.scanning || state.browsing);
    $('analysis-view').classList.toggle('no-results', !node || (!(node.children || []).length && !node.omittedChildren && !hasAccountingRows));
    $('analysis-empty').classList.toggle('is-loading', state.scanning && !entry);
    $('analysis-chart-toggle').textContent = state.showChart ? '收起占用图' : '显示占用图';
    $('analysis-chart-toggle').setAttribute('aria-pressed', String(state.showChart));
    $('analysis-details-toggle').textContent = state.showDetails ? '收起详细信息' : '详细信息';
    $('analysis-details-toggle').setAttribute('aria-expanded', String(state.showDetails));
    $('analysis-details-toggle').setAttribute('aria-pressed', String(state.showDetails));
    $('analysis-chart-toggle').disabled = !node;
    $('analysis-details-toggle').disabled = !node && !state.scanning;
    $('analysis-sort').disabled = busy || !node;
    if (app.getView() === 'analysis') {
      $('page-label').textContent = node ? friendlyName(node) : '按大小查看文件夹';
      $('start-scan').disabled = busy || !state.path || !app.desktop || app.demo;
      $('scan-button-text').textContent = state.browsing ? '打开中…' : state.scanning ? '完整分析中…' : analysisAction;
    }
    $('analysis-location').disabled = busy || !app.desktop || app.demo;
    $('choose-analysis-folder').disabled = busy || !app.desktop || app.demo;
    $('check-cleanup-suggestions').disabled = busy;
    const back = navigation.backAction(state.history, busy || state.scanning);
    $('analysis-back').disabled = back.disabled;
    $('analysis-back').setAttribute('aria-label', back.label);
    $('analysis-back').title = back.label;
    $('analysis-reveal').disabled = !entry || busy || !app.desktop || app.demo;
    $('cancel-analysis').disabled = state.cancelling;
    $('cancel-analysis').textContent = state.cancelling ? '停止中…' : '停止';
    $('analysis-breadcrumbs').innerHTML = state.history.length ? state.history.map((item, index) => `${index ? '<span class="breadcrumb-divider">›</span>' : ''}<button data-history-index="${index}" ${busy || index === state.history.length - 1 ? 'disabled' : ''} title="${escape(state.showDetails ? item.node.path : friendlyName(item.node))}">${escape(friendlyName(item.node))}</button>`).join('') : '<span>空间占用</span>';
    // Keep the current folder visible when its ancestors exceed the toolbar width.
    alignBreadcrumbs();
    renderProgress();
    renderStorageDetails(accounting);
    const permissionCount = Math.max(0, Number(entry?.report.permissionDeniedCount) || 0);
    const blockedCount = Math.max(0, Number(entry?.report.blockedDirectoryCount) || 0);
    const needsAccessReview = permissionCount > 0 || blockedCount > 0;
    $('analysis-permissions').classList.toggle('hidden', !needsAccessReview);
    $('analysis-permissions').disabled = busy || !app.desktop || app.demo;
    if (!needsAccessReview) $('analysis-permission-help').classList.add('hidden');
    if (!node) {
      $('analysis-empty').classList.remove('hidden');
      if (!state.scanning) $('analysis-empty').innerHTML = state.emptyReason ? '<strong>这个位置已无法打开</strong><p>选择其他位置，再开始分析。</p>' : '<strong>查看文件夹占用</strong><p>选择位置后，点击“开始分析”。</p>';
      $('analysis-rows').innerHTML = '';
      $('analysis-status').textContent = '只查看占用，不会修改文件';
      $('analysis-table-footer').classList.remove('has-pages');
      $('analysis-table-footer').textContent = '';
      $('analysis-warnings').classList.add('hidden');
      $('analysis-notices').classList.add('hidden');
      renderMessage();
      return;
    }
    const children = childrenFor(node);
    const pages = Math.max(1, Math.ceil(children.length / PAGE_SIZE));
    state.page = Math.min(Math.max(1, state.page), pages);
    const shown = children.slice((state.page - 1) * PAGE_SIZE, state.page * PAGE_SIZE);
    const omittedBytes = Math.max(0, node.bytes - children.reduce((total, child) => total + child.bytes, 0));
    $('analysis-current-name').textContent = friendlyName(node);
    $('analysis-child-count').textContent = `占用 ${sizeLabel(node)}${node.partial && sizeKnown(node) ? ' · 仅统计已读取' : ''}`;
    $('analysis-rows').innerHTML = shown.map((child) => {
      const directory = child.kind === 'directory';
      const known = sizeKnown(child);
      const knownProportion = known && sizeKnown(node) && !node.stale && !child.stale;
      const proportion = knownProportion && node.bytes > 0 ? child.bytes / node.bytes * 100 : 0;
      const subtitle = !known ? '<small>目录大小尚未统计</small>' : child.stale ? '<small>占用需更新</small>' : child.partial ? '<small>部分未读取</small>' : child.kind === 'symlink' ? '<small>快捷链接</small>' : '';
      const caption = state.showDetails ? child.path : `${directory ? '打开' : '在 Finder 中显示'}${friendlyName(child)}`;
      return `<tr><td><button class="analysis-node-name" data-analysis-node="${escape(child.id)}" ${busy ? 'disabled' : ''} title="${escape(caption)}">${icon(directory ? 'folder' : 'file')}<span>${escape(friendlyName(child))}${subtitle}</span>${directory ? '<b>›</b>' : ''}</button></td><td class="analysis-size">${sizeLabel(child)}</td><td class="analysis-percentage"><span>${knownProportion ? `${proportion < .1 && proportion > 0 ? '&lt;0.1' : proportion.toFixed(1)}%` : '待分析'}</span>${knownProportion ? `<i data-percent="${Math.min(100, proportion)}"></i>` : ''}</td><td class="analysis-file-count">${known ? Number(child.files || 0).toLocaleString() : '—'}</td><td class="analysis-more-action"><button class="row-reveal" data-analysis-reveal="${escape(child.id)}" title="在 Finder 中找到" aria-label="在 Finder 中找到 ${escape(child.name)}" ${busy || !app.desktop || app.demo ? 'disabled' : ''}>${icon('folder')}</button></td></tr>`;
    }).join('') + (node.omittedChildren > 0 && state.page === pages ? `<tr class="analysis-omitted"><td>其他未展开项目<small>${Number(node.omittedChildren).toLocaleString()} 个子项</small></td><td class="analysis-size">${!sizeKnown(node) ? '待分析' : node.partial ? '未知' : formatBytes(omittedBytes)}</td><td colspan="3">${!sizeKnown(node) ? '目录大小尚未统计' : node.partial ? '已读取部分计入总占用；未读取占用未知' : '省略的显示项目已计入总占用'}</td></tr>` : '') + (state.page === pages ? accountingRows(accounting) : '');
    $('analysis-rows').querySelectorAll('[data-percent]').forEach((element) => { element.style.width = `${Number(element.dataset.percent) || 0}%`; });
    $('analysis-empty').classList.toggle('hidden', children.length > 0 || node.omittedChildren > 0 || hasAccountingRows);
    if (!children.length && !node.omittedChildren && !hasAccountingRows) {
      const empty = node.kind === 'directory' && !node.hasChildren && !node.files;
      $('analysis-empty').innerHTML = `<strong>${node.unavailable ? '这个目录已被移动或删除' : node.stale ? '目录列表已更新' : !sizeKnown(node) ? '目录大小尚未统计' : node.partial ? '部分内容未能读取' : node.hasChildren ? '继续查看这个文件夹' : empty ? '这个文件夹为空' : '没有可展示的子项'}</strong><p>${node.unavailable ? '返回上级或选择其他位置查看。' : node.stale ? '占用来自已有统计，重新分析可更新大小。' : !sizeKnown(node) ? '可以选择完整分析，统计这个目录的大小。' : node.partial ? '打开上方提示查看原因，再重新分析。' : node.hasChildren ? `点击“${analysisAction}”，读取里面的文件与文件夹。` : '可以选择其他位置查看。'}</p>`;
    }
    const first = children.length ? (state.page - 1) * PAGE_SIZE + 1 : 0;
    $('analysis-table-footer').classList.toggle('has-pages', pages > 1);
    $('analysis-table-footer').innerHTML = pages > 1 ? `<span>${first}–${Math.min(state.page * PAGE_SIZE, children.length)} / ${children.length} 项</span><div class="pagination"><button class="page-button" data-analysis-page="previous" ${state.page <= 1 || busy ? 'disabled' : ''}>上一页</button><span>${state.page} / ${pages}</span><button class="page-button" data-analysis-page="next" ${state.page >= pages || busy ? 'disabled' : ''}>下一页</button></div>` : '';
    $('analysis-status').textContent = (accounting ? `系统已用 ${formatBytes(accounting.storage.usedBytes)} · ${accounting.differentMeasurement ? '文件块统计' : '已定位文件'} ${formatBytes(accounting.located)} · ${accounting.differentMeasurement ? '计量不同' : `未定位 ${formatBytes(accounting.unlocated)}`}` : `${friendlyName(node)} · ${sizeLabel(node)}${sizeKnown(node) && (entry.browsed || entry.cached) ? ' · 已有统计' : ''}${node.partial && sizeKnown(node) ? ' · 仅统计已读取' : ''} · 不会修改文件`) + (node.stale || entry.stale ? ' · 占用需更新' : '');
    $('analysis-snapshot').textContent = entry.browsed || entry.cached ? `${sizeKnown(node) ? `${Number(node.files || 0).toLocaleString()} 个文件 · 已有统计${measuredTime(entry.report)}` : '目录大小待分析'}${entry.browsed ? ` · 打开耗时 ${(Number(entry.report.durationMs || 0) / 1000).toFixed(1)} 秒` : ''}` : `${Number(entry.report.scannedFiles || 0).toLocaleString()} 个文件 · ${(Number(entry.report.durationMs || 0) / 1000).toFixed(1)} 秒${entry.report.availableBytes >= 0 ? ` · 磁盘可用 ${formatBytes(entry.report.availableBytes)}` : ''}`;
    if (node.stale || entry.stale) $('analysis-snapshot').textContent += ' · 占用需更新';
    const warnings = [...new Set([...(entry.report.warnings || []), ...(entry.report.storage?.warnings || []), ...(typeof entry.report.storageWarning === 'string' ? [entry.report.storageWarning] : [])])];
    if (accounting?.differentMeasurement) warnings.unshift('文件块统计与系统计量不同，可能包含共享块重复计量。当前占用图只展示文件夹统计，不能与系统已用空间直接相减。');
    if (entry.report.cancelled) warnings.unshift('扫描已停止。当前总大小仅包含已经读取的文件。');
    if (node.partial && sizeKnown(node) && !warnings.length) warnings.push('此目录仅统计已读取项目；未读取项目的占用未知。');
    if (!browsingSnapshot && entry.report.scanComplete === false && !warnings.length) warnings.push('本次分析未读取完整；未知空间未计入已定位文件。');
    $('analysis-warnings').classList.toggle('hidden', warnings.length === 0);
    $('analysis-notices').classList.toggle('hidden', warnings.length === 0 && !needsAccessReview);
    $('analysis-warning-summary').textContent = `${entry.report.cancelled ? '本次分析已停止' : node.partial && sizeKnown(node) ? '此目录部分未读取' : !browsingSnapshot && (entry.report.root?.partial || entry.report.scanComplete === false) ? '本次分析有未读取内容' : browsingSnapshot ? '打开目录时有提示' : '检查提示'} · ${warnings.length} 条提示`;
    $('analysis-warning-list').innerHTML = warnings.map((warning) => `<li>${escape(warning)}</li>`).join('');
    if (state.showChart) renderChart(node);
    renderMessage();
  }
  function validReport(report) {
    if (!report || typeof report.analysisId !== 'string' || !report.analysisId || !report.root || (report.warnings !== undefined && !Array.isArray(report.warnings))) return false;
    const pending = [report.root];
    while (pending.length) {
      const node = pending.pop();
      if (!node || typeof node.id !== 'string' || !node.id || typeof node.path !== 'string' || typeof node.name !== 'string' || !['directory', 'file', 'symlink'].includes(node.kind) || !Number.isFinite(node.bytes) || node.bytes < 0 || !Array.isArray(node.children) || (node.sizeKnown !== undefined && typeof node.sizeKnown !== 'boolean')) return false;
      pending.push(...node.children);
    }
    return true;
  }
  async function scanDirectory(path = state.path, append = false, parentHistory = state.history) {
    if (!app.desktop || app.demo || app.isBusy() || !path) return;
    const parents = navigation.parentsForScan(state.history, path, append, parentHistory);
    const revision = ++state.revision;
    state.scanning = true;
    state.cancelling = false;
    state.message = null;
    state.scanPath = path;
    state.progress = null;
    startProgressTimer(revision);
    closeOptions();
    app.setAnalysisBusy(true);
    render();
    try {
      const report = await invoke('analyze_directory', { path });
      if (revision !== state.revision) return;
      if (!validReport(report)) throw new Error('目录分析结果格式无效。');
      // Results opened from an older analysis must not survive a fresh source scan.
      state.browseCache.clear();
      const entry = { node: report.root, report, page: 1 };
      state.history = navigation.commitScan(parents, entry);
      const origins = new Set(state.history.map((item) => originOf(item.report)));
      for (const origin of state.invalidatedNodes.keys()) if (!origins.has(origin)) { state.invalidatedNodes.delete(origin); state.staleDirectories.delete(origin); }
      state.emptyReason = null;
      state.path = report.root.path;
      state.page = 1;
      updateLocations();
    } catch (error) {
      if (revision === state.revision) message('error', `目录扫描未完成：${errorText(error)}${current() ? ' 已保留上次结果。' : ''}`);
    } finally {
      if (revision === state.revision) { state.scanning = false; state.cancelling = false; stopProgressTimer(); app.setAnalysisBusy(false); render(); }
    }
  }
  function rememberBrowse(key, report) {
    state.browseCache.set(key, report);
    if (state.browseCache.size > 128) state.browseCache.delete(state.browseCache.keys().next().value);
  }
  function commitBrowse(parents, report) {
    filterInvalidNodes(report);
    state.history = navigation.commitScan(parents, { node: report.root, report, page: 1, browsed: true, stale: Boolean(report.root.stale) });
    state.path = report.root.path;
    state.page = 1;
    state.emptyReason = null;
    updateLocations();
  }
  async function recoverInvalidNode(target, sourceReport, contextEntry, status, activeRevision = null) {
    invalidateNode(target, sourceReport);
    if (current() !== contextEntry || (activeRevision === null && app.isBusy())) { render(); return; }
    const ownsBusy = activeRevision === null;
    const revision = activeRevision ?? ++state.revision;
    const history = [...state.history];
    const selectedPath = state.path;
    state.browsing = true;
    state.recovering = true;
    state.cancelling = false;
    state.message = null;
    stopProgressTimer();
    if (ownsBusy) app.setAnalysisBusy(true);
    try {
      for (let index = history.length - 1; index >= 0; index -= 1) {
        const candidate = history[index];
        if (candidate.invalid || candidate.node.unavailable) continue;
        state.scanPath = candidate.node.path;
        render();
        let report;
        try {
          report = await invoke('browse_analysis_directory', { analysisId: candidate.report.analysisId, nodeId: candidate.node.id });
        } catch (error) {
          if (revision !== state.revision) return;
          const invalid = invalidStatus(error);
          if (invalid) { invalidateNode(candidate.node, candidate.report); continue; }
          if (current()?.invalid) {
            state.history = history.slice(0, index + 1);
            state.path = candidate.node.path;
            state.page = candidate.page || 1;
            updateLocations();
          }
          message('info', `这个项目已变化。${friendlyFailure(error, '刷新列表', true)}`);
          return;
        }
        if (revision !== state.revision) return;
        if (!validReport(report) || report.root.id !== candidate.node.id || report.root.path !== candidate.node.path || report.root.kind !== 'directory') {
          message('info', '这个项目已变化，列表暂时无法更新。重新分析可更新占用。');
          return;
        }
        if (state.cancelling || report.cancelled) { message('info', '已停止更新列表，已有统计需要重新分析。'); return; }
        if (listingFailed(report)) {
          if (current()?.invalid) { state.history = history.slice(0, index + 1); state.path = candidate.node.path; state.page = candidate.page || 1; updateLocations(); }
          message('info', '这个项目已变化。' + (Number(report.permissionDeniedCount) > 0 ? '暂时无法读取列表，请检查访问权限后重试。' : '暂时无法读取列表，请稍后重试。') + '已保留已有结果。');
          return;
        }
        report.root.stale = true;
        commitBrowse(history.slice(0, index), report);
        if (index === history.length - 1 && selectedPath !== contextEntry.node.path) { state.path = selectedPath; updateLocations(); }
        rememberBrowse(`${candidate.report.analysisId}\u0000${candidate.node.id}`, report);
        message('info', index < history.length - 1 ? '目录已变化，已返回上级。重新分析可更新占用。' : Number(report.blockedDirectoryCount) > 0 ? '这个项目已变化，旧条目已移除。当前显示已有统计。' : status === 'changed' ? '这个项目已变化，列表已更新。重新分析可更新占用。' : '这个项目已被移动或删除，列表已更新。');
        return;
      }
      state.history = [];
      state.path = '';
      state.page = 1;
      state.emptyReason = status;
      updateLocations();
      message('info', '这个位置已被移动或删除，请选择其他位置。');
    } finally {
      if (revision === state.revision) {
        state.recovering = false;
        if (ownsBusy) { state.browsing = false; state.cancelling = false; app.setAnalysisBusy(false); }
        render();
      }
    }
  }
  async function browseDirectory(node, parents, sourceReport) {
    if (!app.desktop || app.demo || app.isBusy() || !sourceReport?.analysisId) return;
    const cacheKey = `${sourceReport.analysisId}\u0000${node.id}`;
    const cached = state.browseCache.get(cacheKey);
    if (cached) {
      await openCachedDirectory(node, parents, sourceReport, cached);
      return;
    }
    const contextEntry = current();
    const revision = ++state.revision;
    state.browsing = true;
    state.cancelling = false;
    state.message = null;
    state.scanPath = node.path;
    state.progress = null;
    stopProgressTimer();
    closeOptions();
    app.setAnalysisBusy(true);
    render();
    try {
      const report = await invoke('browse_analysis_directory', { analysisId: sourceReport.analysisId, nodeId: node.id });
      if (revision !== state.revision) return;
      if (!validReport(report) || report.root.id !== node.id || report.root.path !== node.path || report.root.kind !== 'directory') throw new Error('打开目录返回的结果格式无效。');
      if (state.cancelling || report.cancelled) { message('info', '已停止打开目录，保留当前结果。'); return; }
      if (listingFailed(report)) { message('error', (Number(report.permissionDeniedCount) > 0 ? '暂时无法读取这个目录，请检查访问权限后重试。' : '暂时无法读取这个目录，请稍后重试。') + '已保留当前结果。'); return; }
      commitBrowse(parents, report);
      rememberBrowse(cacheKey, report);
    } catch (error) {
      if (revision === state.revision) {
        const invalid = invalidStatus(error);
        if (invalid) await recoverInvalidNode(node, sourceReport, contextEntry, invalid, revision);
        else message('error', friendlyFailure(error, '打开这个目录', true));
      }
    } finally {
      if (revision === state.revision) { state.browsing = false; state.cancelling = false; app.setAnalysisBusy(false); render(); }
    }
  }
  async function openCachedDirectory(node, parents, sourceReport, cachedReport = null, cachedHistory = null) {
    if (!app.desktop || app.demo || app.isBusy()) return;
    const contextEntry = current();
    const revision = ++state.revision;
    state.browsing = true;
    state.cancelling = false;
    state.message = null;
    state.scanPath = node.path;
    closeOptions();
    app.setAnalysisBusy(true);
    render();
    try {
      const result = await invoke('inspect_analysis_node', { analysisId: sourceReport.analysisId, nodeId: node.id });
      if (revision !== state.revision) return;
      if (state.cancelling) { message('info', '已停止打开目录，保留当前结果。'); return; }
      if (result?.status === 'missing' || result?.status === 'changed') {
        await recoverInvalidNode(node, sourceReport, contextEntry, result.status, revision);
        return;
      }
      if (result?.status !== 'ready') throw new Error('无法验证目录状态。');
      if (node.unavailable || state.invalidatedNodes.get(originOf(sourceReport))?.has(node.id)) {
        await recoverInvalidNode(node, sourceReport, contextEntry, 'changed', revision);
        return;
      }
      if (cachedReport) commitBrowse(parents, cachedReport);
      else {
        for (const item of cachedHistory.slice(state.history.length)) item.cached = true;
        state.history = cachedHistory;
        state.path = node.path;
        state.page = 1;
        state.emptyReason = null;
        updateLocations();
      }
    } catch (error) {
      if (revision === state.revision) {
        const invalid = invalidStatus(error);
        if (invalid) await recoverInvalidNode(node, sourceReport, contextEntry, invalid, revision);
        else message('error', friendlyFailure(error, '打开这个目录', true));
      }
    } finally {
      if (revision === state.revision) { state.browsing = false; state.cancelling = false; app.setAnalysisBusy(false); render(); }
    }
  }
  async function cancel() {
    if ((!state.scanning && !state.browsing) || state.cancelling || !app.desktop) return;
    const revision = state.revision;
    state.cancelling = true;
    render();
    try { await invoke('cancel_analysis'); }
    catch (error) { if (revision === state.revision && (state.scanning || state.browsing)) { state.cancelling = false; message('error', `停止操作失败：${errorText(error)}`); render(); } }
  }
  function findNode(id) {
    const root = current()?.node;
    if (!root) return null;
    const queue = [root];
    while (queue.length) { const node = queue.pop(); if (node.id === id) return node; queue.push(...(node.children || [])); }
    return null;
  }
  async function reveal(node = current()?.node) {
    const entry = current();
    if (!node || !entry || !app.desktop || app.demo || app.isBusy()) return;
    if (!node.path) { message('info', '此文件名无法用 UTF-8 表示，已统计占用，但不能从应用定位。'); return; }
    const revision = state.revision;
    const selectedPath = state.path;
    try {
      const result = await invoke('reveal_analysis_node', { analysisId: entry.report.analysisId, nodeId: node.id });
      if (result?.status === 'missing' || result?.status === 'changed') {
        if (current() !== entry || revision !== state.revision || state.path !== selectedPath) { invalidateNode(node, entry.report); render(); return; }
        await recoverInvalidNode(node, entry.report, entry, result.status);
      }
    } catch (error) {
      if (current() === entry && revision === state.revision) message('error', friendlyFailure(error, '在 Finder 中显示'));
    }
  }
  function drill(node) {
    if (!node || state.scanning || app.isBusy()) return;
    if (!node.path) { message('info', '此文件名无法用 UTF-8 表示，已统计占用，但不能从应用继续打开。'); return; }
    if (node.kind !== 'directory') { reveal(node); return; }
    const entry = current();
    if (entry) entry.page = state.page;
    const plan = navigation.drillPlan(state.history, node);
    if (!plan) return;
    if (plan.needsScan) { browseDirectory(node, plan.parents, entry.report); return; }
    openCachedDirectory(node, plan.parents, entry.report, null, plan.history);
  }
  function goBack(index) {
    if (state.scanning || app.isBusy()) return;
    if (index === undefined) {
      const back = navigation.backAction(state.history);
      if (back.kind === 'home') { app.showHome(); return; }
      index = back.index;
    }
    const history = navigation.returnTo(state.history, index);
    if (history === state.history) return;
    state.history = history;
    state.path = current().node.path;
    state.page = current().page || 1;
    state.message = null;
    updateLocations();
    render();
  }
  async function chooseFolder() {
    if (!app.desktop || app.demo || app.isBusy()) return;
    try { const path = await invoke('choose_analysis_directory'); if (path) setPath(path); }
    catch (error) { message('error', `无法打开文件夹选择器：${errorText(error)}`); }
  }
  async function openPrivacySettings() {
    if (!app.desktop || app.demo || app.isBusy()) return;
    $('analysis-permission-help').classList.remove('hidden');
    $('analysis-options').open = true;
    try { await invoke('open_privacy_settings'); }
    catch (error) { message('error', `无法打开系统设置：${errorText(error)}。请按“更多”中的路径手动打开。`); }
  }
  function bind() {
    $('analysis-permissions').addEventListener('click', openPrivacySettings);
    $('analysis-chart-toggle').addEventListener('click', () => { state.showChart = !state.showChart; closeOptions(true); render(); });
    $('analysis-details-toggle').addEventListener('click', () => { state.showDetails = !state.showDetails; closeOptions(true); render(); });
    $('analysis-location').addEventListener('change', (event) => setPath(event.target.value));
    $('choose-analysis-folder').addEventListener('click', chooseFolder);
    $('cancel-analysis').addEventListener('click', cancel);
    $('analysis-back').addEventListener('click', () => goBack());
    $('analysis-reveal').addEventListener('click', () => { closeOptions(true); reveal(); });
    $('analysis-sort').addEventListener('change', (event) => { state.sort = event.target.value; state.page = 1; closeOptions(true); render(); });
    $('check-cleanup-suggestions').addEventListener('click', () => { if (!app.isBusy()) app.showCleanupAndScan(); });
    $('analysis-breadcrumbs').addEventListener('click', (event) => { const button = event.target.closest('[data-history-index]'); if (button) goBack(Number(button.dataset.historyIndex)); });
    $('analysis-view').addEventListener('click', (event) => {
      const accountingButton = event.target.closest('[data-analysis-accounting]');
      if (accountingButton) { $('analysis-options').open = true; $('analysis-storage-info').open = true; $('analysis-storage-info').querySelector('summary').focus(); return; }
      const cancelButton = event.target.closest('[data-analysis-cancel]');
      if (cancelButton && !cancelButton.disabled) { cancel(); return; }
      const revealButton = event.target.closest('[data-analysis-reveal]');
      if (revealButton) { reveal(findNode(revealButton.dataset.analysisReveal)); return; }
      const nodeButton = event.target.closest('[data-analysis-node]');
      if (nodeButton) drill(state.chartNodes.get(nodeButton.dataset.analysisNode) || findNode(nodeButton.dataset.analysisNode));
    });
    $('analysis-chart').addEventListener('keydown', (event) => { if (event.key === 'Enter' || event.key === ' ') { const segment = event.target.closest('[data-analysis-node]'); if (segment) { event.preventDefault(); drill(state.chartNodes.get(segment.dataset.analysisNode)); } } });
    $('analysis-table-footer').addEventListener('click', (event) => { const button = event.target.closest('[data-analysis-page]'); if (!button || button.disabled || app.isBusy()) return; state.page += button.dataset.analysisPage === 'next' ? 1 : -1; render(); $('analysis-rows').closest('.analysis-table-scroll').scrollTop = 0; });
    document.addEventListener('click', (event) => { if (!event.target.closest('#analysis-options, [data-analysis-accounting], #analysis-permissions')) closeOptions(); });
    document.addEventListener('keydown', (event) => { if (event.key === 'Escape' && $('analysis-options').open) closeOptions(true); });
    window.addEventListener('mac-sweep-view', () => render());
    window.addEventListener('resize', alignBreadcrumbs);
    window.addEventListener('beforeunload', stopProgressTimer);
  }
  async function init() {
    bind();
    render();
    if (app.demo) { $('analysis-demo').classList.remove('hidden'); message('info', '目录分析演示未加载本机数据。清理示例可从侧栏“全部项目”查看。'); return; }
    if (!app.desktop) { message('info', '请在 Mac Sweep 桌面应用中选择目录并扫描。浏览器不会读取本机文件。'); return; }
    const results = await Promise.allSettled([
      invoke('get_analysis_locations'),
      window.__TAURI__.event.listen('analysis-progress', (event) => {
        if (!state.scanning) return;
        const progress = event.payload;
        if (!progress || typeof progress !== 'object') return;
        const finiteCount = (value) => typeof value === 'number' && Number.isFinite(value) && value >= 0;
        if (!finiteCount(progress.scannedFiles) && !finiteCount(progress.bytesFound) && (typeof progress.currentPath !== 'string' || !progress.currentPath)) return;
        const previous = state.progress || {};
        state.progress = {
          currentPath: typeof progress.currentPath === 'string' && progress.currentPath ? progress.currentPath : previous.currentPath || state.scanPath,
          scannedFiles: finiteCount(progress.scannedFiles) ? Math.floor(progress.scannedFiles) : previous.scannedFiles || 0,
          bytesFound: finiteCount(progress.bytesFound) ? progress.bytesFound : previous.bytesFound || 0,
        };
        state.lastProgressAt = performance.now();
        renderProgress();
      }),
    ]);
    if (results[0].status === 'fulfilled' && Array.isArray(results[0].value)) {
      state.locations = results[0].value;
      state.path = state.path || state.locations.find((entry) => entry.id === 'home')?.path || state.locations[0]?.path || '';
      updateLocations();
    } else message('error', `无法读取扫描位置：${results[0].status === 'rejected' ? errorText(results[0].reason) : '返回格式无效'}。可以手动选择文件夹。`);
    if (results[1].status === 'rejected') {
      if (state.message?.type === 'error') message('error', `${state.message.text} 进度通知也不可用，大小结果会在完成后显示。`);
      else message('info', `进度通知不可用：${errorText(results[1].reason)}。大小结果会在完成后显示。`);
    }
    render();
  }
  window.macSweepAnalyzer = { scan: () => scanDirectory(), cancel };
  init();
})();
