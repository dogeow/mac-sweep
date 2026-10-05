const test = require('node:test');
const assert = require('node:assert/strict');
const navigation = require('../frontend/analysis-navigation.js');

const folder = (id, path, children = [], overrides = {}) => ({
  id, path, name: path.split('/').pop(), kind: 'directory', bytes: 4096,
  children, hasChildren: children.length > 0, ...overrides,
});
const entry = (node, analysisId = 'original', overrides = {}) => ({
  node, report: { analysisId, root: node, scannedFiles: 100 }, page: 1, ...overrides,
});

test('idle and scan roots return home, while busy navigation stays disabled', () => {
  const root = entry(folder('root', '/example'));
  for (const history of [[], [root]]) {
    assert.deepEqual(navigation.backAction(history), {
      kind: 'home', index: null, label: '返回我的 Mac', disabled: false,
    });
    assert.equal(navigation.backAction(history, true).disabled, true);
  }
  const child = entry(folder('child', '/example/child'));
  assert.deepEqual(navigation.backAction([root, child]), {
    kind: 'directory', index: 0, label: '返回上一级', disabled: false,
  });
  assert.equal(navigation.backAction([root, child], true).disabled, true);
});

test('directory entry refresh plans preserve parent data and pages until a reply commits', () => {
  const deep = folder('deep', '/example/child/deep');
  const child = folder('child', '/example/child', [deep]);
  const root = entry(folder('root', '/example', [child], { bytes: 9000 }), 'original', { page: 3 });
  const initial = [root];
  const first = navigation.drillPlan(initial, child);
  assert.equal(first.needsScan, true);
  assert.strictEqual(first.history, initial);
  const refreshedChild = entry(child, 'fresh-child');
  const childHistory = navigation.commitScan(first.parents, refreshedChild);
  const second = navigation.drillPlan(childHistory, deep);
  assert.equal(second.needsScan, true);
  assert.strictEqual(second.history, childHistory);
  const deepHistory = navigation.commitScan(second.parents, entry(deep, 'fresh-deep'));
  assert.equal(initial.length, 1);
  const parentHistory = navigation.returnTo(deepHistory);
  assert.strictEqual(navigation.current(parentHistory).node, child);
  assert.strictEqual(navigation.current(parentHistory).report, refreshedChild.report);
  const rootHistory = navigation.returnTo(parentHistory);
  assert.strictEqual(navigation.current(rootHistory), root);
  assert.equal(navigation.current(rootHistory).page, 3);
  assert.equal(navigation.current(rootHistory).node.bytes, 9000);
  assert.equal(navigation.backAction(rootHistory).kind, 'home');
});

test('deep unexpanded chart targets retain cached intermediate folders across rescans', () => {
  const omitted = folder('omitted', '/example/child/deep', [], { hasChildren: true });
  const child = folder('child', '/example/child', [omitted]);
  const root = entry(folder('root', '/example', [child]));
  const original = [root];
  const plan = navigation.drillPlan(original, omitted);
  assert.equal(plan.needsScan, true);
  assert.strictEqual(plan.history, original);
  assert.deepEqual(plan.parents.map((item) => item.node.path), ['/example', '/example/child']);

  const deeper = folder('deeper', '/example/child/deep/deeper', [], { hasChildren: true });
  const deepResult = entry(folder('deep-new', omitted.path, [deeper]), 'deep-scan');
  const parents = navigation.parentsForScan(original, plan.path, true, plan.parents);
  const deepHistory = navigation.commitScan(parents, deepResult);
  const nextPlan = navigation.drillPlan(deepHistory, deeper);
  const deeperResult = entry(folder('deeper-new', deeper.path), 'deeper-scan');
  const deeperHistory = navigation.commitScan(nextPlan.parents, deeperResult);

  const backToDeep = navigation.returnTo(deeperHistory);
  assert.strictEqual(navigation.current(backToDeep), deepResult);
  const backToChild = navigation.returnTo(backToDeep);
  assert.strictEqual(navigation.current(backToChild).node, child);
  assert.strictEqual(navigation.current(backToChild).report, root.report);
  assert.strictEqual(navigation.current(navigation.returnTo(backToChild)), root);
});

test('refreshing a nested folder keeps its parent and new locations start a new root', () => {
  const child = folder('child', '/example/child');
  const root = entry(folder('root', '/example', [child]));
  const plan = navigation.drillPlan([root], child);
  const history = navigation.commitScan(plan.parents, entry(child));
  const refreshed = entry(folder('child-new', child.path, [], { bytes: 8192 }), 'refresh');
  const refreshedHistory = navigation.commitScan(
    navigation.parentsForScan(history, child.path), refreshed,
  );
  assert.strictEqual(navigation.current(refreshedHistory), refreshed);
  assert.strictEqual(navigation.current(navigation.returnTo(refreshedHistory)), root);

  const newParents = navigation.parentsForScan(refreshedHistory, '/another');
  assert.deepEqual(newParents, []);
  // Starting or failing a scan cannot mutate the currently displayed history.
  assert.strictEqual(navigation.current(refreshedHistory), refreshed);
  const another = entry(folder('another', '/another'), 'another-scan');
  const anotherHistory = navigation.commitScan(newParents, another);
  assert.deepEqual(anotherHistory, [another]);
  assert.equal(navigation.backAction(anotherHistory).kind, 'home');
});

test('previously empty and already expanded folders both request fresh shallow entries', () => {
  const empty = folder('empty', '/example/empty', [], { hasChildren: false });
  const expanded = folder('expanded', '/example/expanded', [folder('file-dir', '/example/expanded/nested')]);
  const root = entry(folder('root', '/example', [empty, expanded]));
  const history = [root];
  for (const child of [empty, expanded]) {
    const plan = navigation.drillPlan(history, child);
    assert.equal(plan.needsScan, true);
    assert.strictEqual(plan.history, history);
    assert.deepEqual(plan.parents, [root]);
  }
});

test('invalid targets and current breadcrumbs cannot create phantom history entries', () => {
  const child = folder('child', '/example/child');
  const root = entry(folder('root', '/example', [child]));
  const history = [root];
  assert.equal(navigation.drillPlan(history, root.node), null);
  assert.equal(navigation.drillPlan(history, folder('stale', '/elsewhere')), null);
  assert.strictEqual(navigation.returnTo(history, 0), history);
  assert.strictEqual(navigation.returnTo(history, -1), history);
  assert.strictEqual(navigation.returnTo(history, 0.5), history);

  const refreshedRoot = entry(folder('root-new', root.node.path), 'root-refresh');
  const committed = navigation.commitScan(history, refreshedRoot);
  assert.deepEqual(committed, [refreshedRoot]);
});
