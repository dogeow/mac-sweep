(function (root) {
  'use strict';
  const fallback = { cache: '其他应用缓存', logs: '其他旧日志', installer: '下载的安装包', orphan: '未识别的应用数据' };
  const size = (item) => Math.max(0, Number(item.bytes) || 0);
  function groupItems(items, sort = 'size-desc') {
    const groups = new Map();
    for (const item of items) {
      const suggested = item.selectedByDefault === true;
      const name = item.appName || fallback[item.category] || '其他文件';
      const key = JSON.stringify([item.category, suggested, name]);
      if (!groups.has(key)) groups.set(key, { key, name, category: item.category, suggested, items: [], bytes: 0, modifiedAt: Infinity });
      const group = groups.get(key);
      group.items.push(item);
      group.bytes += size(item);
      const modified = Number(item.modifiedAt) || 0;
      if (modified > 0) group.modifiedAt = Math.min(group.modifiedAt, modified);
    }
    return [...groups.values()].sort((a, b) => {
      if (a.suggested !== b.suggested) return a.suggested ? -1 : 1;
      if (sort === 'name-asc') return String(a.name).localeCompare(String(b.name), 'zh-CN');
      if (sort === 'date-asc') return a.modifiedAt - b.modifiedAt || b.bytes - a.bytes;
      return (sort === 'size-asc' ? a.bytes - b.bytes : b.bytes - a.bytes) || String(a.name).localeCompare(String(b.name), 'zh-CN');
    });
  }
  function selectedState(group, selected) {
    const count = group.items.filter((item) => selected.has(item.id)).length;
    return { count, checked: group.items.length > 0 && count === group.items.length, indeterminate: count > 0 && count < group.items.length };
  }
  function pageItems(items, requested = 1, pageSize = 100) {
    const pages = Math.max(1, Math.ceil(items.length / pageSize));
    const page = Math.max(1, Math.min(pages, Number(requested) || 1));
    const offset = (page - 1) * pageSize;
    return { page, pages, items: items.slice(offset, offset + pageSize), first: items.length ? offset + 1 : 0, last: Math.min(offset + pageSize, items.length) };
  }
  const api = Object.freeze({ groupItems, selectedState, pageItems });
  if (typeof module === 'object' && module.exports) module.exports = api;
  else root.MacSweepCleanup = api;
})(typeof window === 'object' ? window : globalThis);
