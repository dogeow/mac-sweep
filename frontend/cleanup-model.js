(function (root) {
  'use strict';
  const fallback = { cache: '其他应用缓存', logs: '其他旧日志', installer: '下载的安装包', orphan: '未识别的应用数据' };
  const size = (item) => Number.isFinite(Number(item.bytes)) ? Math.max(0, Number(item.bytes)) : 0;
  const fileCount = (item) => item.files == null ? 1 : Math.max(0, Number(item.files) || 0);
  const directoryHint = (item) => item.kind === 'directory' || item.isDirectory === true || (Number.isFinite(item.files) && item.files !== 1) || String(item.path || '').endsWith('/');
  const pathOf = (parts) => `/${parts.join('/')}`;
  function pathParts(path) {
    if (typeof path !== 'string' || !path.startsWith('/') || path.includes('\0')) return null;
    const parts = path.split('/').filter((part) => part && part !== '.');
    return parts.includes('..') ? null : parts;
  }
  function uniqueItems(items) {
    const seen = new Set();
    return items.filter((item) => {
      if (!item || typeof item.id !== 'string' || !item.id || seen.has(item.id)) return false;
      seen.add(item.id);
      return true;
    });
  }
  function anchor(parts) {
    for (let index = 0; index + 2 < parts.length; index += 1) {
      if (parts[index] !== 'Users') continue;
      // Shared download caches and sandbox caches have explicit roots. Group
      // candidate-only branches under those roots, not their hash prefixes.
      const relative = parts.slice(index + 2);
      const cacheRoots = [
        ['.npm', '_cacache'], ['.cargo', 'registry', 'cache'],
        ['go', 'pkg', 'mod', 'cache', 'download'], ['.gradle', 'caches'],
        ['Library', 'pnpm', 'store'], ['Library', 'Developer', 'Xcode', 'DerivedData'],
      ];
      const cacheRoot = cacheRoots.find((root) => root.every((name, offset) => relative[offset] === name));
      if (cacheRoot) return { end: index + 1 + cacheRoot.length, owner: false };
      if (relative[0] === 'Library' && relative[1] === 'Containers' && relative[2] && relative[3] === 'Data' && relative[4] === 'Library' && relative[5] === 'Caches') return { end: index + 7, owner: false };
      if (parts[index + 2] === 'Library' && ['Caches', 'Logs', 'Application Support', 'Saved Application State', 'Preferences'].includes(parts[index + 3])) return { end: index + 3, owner: parts[index + 3] !== 'Preferences' };
      if (parts[index + 2] === '.cache') return { end: index + 2, owner: true };
      if (parts[index + 2] === 'Downloads') return { end: index + 2, owner: false };
    }
    return null;
  }
  function ownerName(item) {
    if (typeof item.appName === 'string' && item.appName.trim()) return item.appName;
    const parts = pathParts(item.path);
    const base = parts && anchor(parts);
    if (base?.owner && parts[base.end + 1] && (parts.length > base.end + 2 || directoryHint(item))) return parts[base.end + 1];
    return fallback[item.category] || '其他文件';
  }
  function partitions(items) {
    const groups = new Map();
    for (const item of uniqueItems(items)) {
      const suggested = item.selectedByDefault === true;
      const name = ownerName(item);
      const key = JSON.stringify([item.category, suggested, name]);
      if (!groups.has(key)) groups.set(key, { key, name, category: item.category, suggested, items: [], bytes: 0, modifiedAt: Infinity });
      const group = groups.get(key);
      group.items.push(item);
      const modified = Number(item.modifiedAt) || 0;
      if (modified > 0) group.modifiedAt = Math.min(group.modifiedAt, modified);
    }
    return [...groups.values()];
  }
  function compare(sort, a, b) {
    const names = String(a.name || a.path || a.id).localeCompare(String(b.name || b.path || b.id), 'zh-CN');
    if (sort === 'name-asc') return names;
    if (sort === 'date-asc') return (Number(a.modifiedAt) || Infinity) - (Number(b.modifiedAt) || Infinity) || b.bytes - a.bytes || names;
    return (sort === 'size-asc' ? a.bytes - b.bytes : b.bytes - a.bytes) || names;
  }
  // Candidate-only totals. An original directory candidate already contains its
  // descendants: count that covered space once, while retaining every candidate ID.
  function candidateTotals(items) {
    const root = { path: '/', children: new Map(), own: [] };
    const paths = new Map();
    const directoryPaths = new Set();
    let unknownBytes = 0, unknownFiles = 0;
    for (const item of items) {
      const parts = pathParts(item.path);
      if (!parts) { unknownBytes += size(item); unknownFiles += fileCount(item); continue; }
      let node = root;
      for (const name of parts) {
        if (!node.children.has(name)) node.children.set(name, { path: `${node.path === '/' ? '' : node.path}/${name}`, children: new Map(), own: [] });
        node = node.children.get(name);
      }
      node.own.push(item);
    }
    function measure(node) {
      let bytes = 0, files = 0;
      for (const child of node.children.values()) { const value = measure(child); bytes += value.bytes; files += value.fileCount; }
      if (node.own.length) {
        bytes = Math.max(bytes, ...node.own.map(size));
        files = Math.max(files, ...node.own.map(fileCount));
        if (node.children.size || node.own.some(directoryHint)) directoryPaths.add(node.path);
      }
      const total = { bytes, fileCount: files };
      paths.set(node.path, total);
      return total;
    }
    const total = measure(root);
    return { bytes: total.bytes + unknownBytes, fileCount: total.fileCount + unknownFiles, unknownBytes, unknownFiles, paths, directoryPaths };
  }
  function groupItems(items, sort = 'size-desc') {
    const groups = partitions(items);
    for (const group of groups) { const total = candidateTotals(group.items); group.bytes = total.bytes; group.fileCount = total.fileCount; group.candidateCount = group.items.length; }
    return groups.sort((a, b) => {
      if (a.suggested !== b.suggested) return a.suggested ? -1 : 1;
      return compare(sort, a, b);
    });
  }
  function buildDirectoryTree(items, sort = 'size-desc') {
    const forest = [];
    for (const group of partitions(items)) {
      const total = candidateTotals(group.items);
      const records = group.items.map((item) => {
        const parts = pathParts(item.path);
        if (!parts) return { item, parts: null, folder: null };
        const folder = directoryHint(item) || total.directoryPaths.has(pathOf(parts)) ? parts : parts.slice(0, -1);
        const base = anchor(folder);
        let scope = base ? folder.slice(0, Math.min(folder.length, base.end + (base.owner ? 2 : 1))) : folder;
        for (let count = 0; count <= scope.length; count += 1) {
          if (total.directoryPaths.has(pathOf(scope.slice(0, count)))) { scope = scope.slice(0, count); break; }
        }
        return { item, parts, folder, scope };
      });
      const scopes = new Map(records.filter((record) => record.parts).map((record) => [pathOf(record.scope), record.scope]));
      const roots = new Map();
      const makeNode = (path) => ({ key: JSON.stringify(['directory', group.key, path]), path, paths: path ? [path] : [], name: path ? path.split('/').filter(Boolean).pop() || '/' : '其他文件', children: new Map(), files: [] });
      for (const [path, parts] of scopes) {
        if (!parts.some((_, index) => scopes.has(pathOf(parts.slice(0, index))))) roots.set(path, makeNode(path));
      }
      for (const record of records) {
        if (!record.parts) {
          if (!roots.has('')) roots.set('', makeNode(''));
          roots.get('').files.push(record.item);
          continue;
        }
        let node, depth;
        for (depth = 0; depth <= record.scope.length; depth += 1) {
          node = roots.get(pathOf(record.scope.slice(0, depth)));
          if (node) break;
        }
        for (let index = depth; index < record.folder.length; index += 1) {
          const name = record.folder[index];
          if (!node.children.has(name)) node.children.set(name, makeNode(pathOf(record.folder.slice(0, index + 1))));
          node = node.children.get(name);
        }
        node.files.push(record.item);
      }
      function finish(node) {
        node.children = [...node.children.values()].map(finish).sort((a, b) => compare(sort, a, b));
        node.files.sort((a, b) => compare(sort, a, b));
        node.items = [...node.files, ...node.children.flatMap((child) => child.items)];
        node.itemIds = node.items.map((item) => item.id);
        node.candidateCount = node.items.length;
        const measured = node.path ? total.paths.get(node.path) : { bytes: total.unknownBytes, fileCount: total.unknownFiles };
        node.bytes = measured?.bytes || 0;
        node.fileCount = measured?.fileCount || 0;
        node.modifiedAt = Math.min(...node.items.map((item) => Number(item.modifiedAt) > 0 ? Number(item.modifiedAt) : Infinity));
        while (!node.files.length && node.children.length === 1) {
          const child = node.children[0];
          node.name += ` / ${child.name}`;
          node.path = child.path;
          node.paths.push(...child.paths);
          node.children = child.children;
          node.files = child.files;
        }
        return node;
      }
      forest.push(...[...roots.values()].map(finish));
    }
    return forest.sort((a, b) => compare(sort, a, b));
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
  const api = Object.freeze({ groupItems, buildDirectoryTree, selectedState, pageItems });
  if (typeof module === 'object' && module.exports) module.exports = api;
  else root.MacSweepCleanup = api;
})(typeof window === 'object' ? window : globalThis);
