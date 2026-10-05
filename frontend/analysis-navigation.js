(function (root) {
  'use strict';

  const current = (history) => history[history.length - 1] || null;

  function backAction(history, busy = false) {
    return history.length > 1
      ? { kind: 'directory', index: history.length - 2, label: '返回上一级', disabled: Boolean(busy) }
      : { kind: 'home', index: null, label: '返回我的 Mac', disabled: Boolean(busy) };
  }

  function parentsForScan(history, path, append = false, parentHistory = history) {
    if (append) return [...parentHistory];
    // Refreshing the current folder keeps its ancestors. Choosing a different
    // scan location starts a new root when the new result is actually available.
    return current(history)?.node.path === path ? history.slice(0, -1) : [];
  }

  function commitScan(parents, entry) {
    const history = [...parents];
    if (current(history)?.node.path === entry.node.path) history.pop();
    return [...history, entry];
  }

  function nodePath(rootNode, id) {
    if (!rootNode) return null;
    if (rootNode.id === id) return [rootNode];
    for (const child of rootNode.children || []) {
      const path = nodePath(child, id);
      if (path) return [rootNode, ...path];
    }
    return null;
  }

  function drillPlan(history, node) {
    const entry = current(history);
    if (!entry || !node?.path || node.kind !== 'directory') return null;
    const path = nodePath(entry.node, node.id);
    if (!path || path.length < 2) return null;
    const intermediate = path.slice(1, -1).map((parent) => ({ node: parent, report: entry.report, page: 1 }));
    const parents = [...history, ...intermediate];
    // Entering a directory always refreshes its immediate entries. Prior
    // children (including a prior empty result) are a snapshot, not evidence
    // that Finder has left the directory unchanged. The caller browses shallowly.
    const needsScan = true;
    return {
      path: node.path,
      parents,
      needsScan,
      history,
    };
  }

  function returnTo(history, index = history.length - 2) {
    if (!Number.isInteger(index) || index < 0 || index >= history.length - 1) return history;
    return history.slice(0, index + 1);
  }

  const api = { current, backAction, parentsForScan, commitScan, drillPlan, returnTo };
  if (typeof module !== 'undefined' && module.exports) module.exports = api;
  if (root) root.macSweepAnalysisNavigation = api;
})(typeof window === 'undefined' ? null : window);
