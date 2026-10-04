const test = require('node:test');
const assert = require('node:assert/strict');
const { groupItems, buildDirectoryTree, selectedState, pageItems } = require('../frontend/cleanup-model.js');

const item = (id, overrides = {}) => ({ id, appName: 'Example', category: 'cache', selectedByDefault: true, bytes: 4096, modifiedAt: 100, ...overrides });

test('full grouping keeps all file pages together and review items separate', () => {
  const files = Array.from({ length: 201 }, (_, i) => item(`file-${i}`));
  files.push(item('review', { selectedByDefault: false }), item('review-low', { selectedByDefault: false, risk: 'low' }));
  const groups = groupItems(files);
  assert.equal(groups.length, 2);
  assert.equal(groups[0].items.length, 201);
  assert.equal(groups[0].bytes, 201 * 4096);
  assert.equal(groups[1].suggested, false);
  assert.deepEqual(groups[1].items.map((i) => i.id), ['review', 'review-low']);
  assert.equal(pageItems(groups[0].items, 3).items.length, 1);
  assert.equal(pageItems(groups[0].items, 99).page, 3);
});

test('whole-group selection includes hidden detail pages and stays indeterminate after one exclusion', () => {
  const group = groupItems(Array.from({ length: 201 }, (_, i) => item(`file-${i}`)))[0];
  const selected = new Set(group.items.map((i) => i.id));
  assert.deepEqual(selectedState(group, selected), { count: 201, checked: true, indeterminate: false });
  selected.delete('file-150');
  assert.deepEqual(selectedState(group, selected), { count: 200, checked: false, indeterminate: true });
  assert.equal(pageItems(group.items, 2).items.some((i) => i.id === 'file-150'), true);
});

test('filtered groups operate only on matching IDs while global selection survives filters', () => {
  const files = [item('shown'), item('hidden'), item('manual', { selectedByDefault: false })];
  const selected = new Set(['shown', 'hidden']);
  const filtered = groupItems(files.filter((i) => i.id === 'shown'))[0];
  filtered.items.forEach((i) => selected.delete(i.id));
  assert.deepEqual([...selected], ['hidden']);
  assert.equal(selected.has('manual'), false);
});

test('group-level pagination never splits an app group or its total', () => {
  const files = Array.from({ length: 61 }, (_, i) => item(`${i}`, { appName: `App ${i}` }));
  const groups = groupItems(files);
  const page = pageItems(groups, 3, 30);
  assert.equal(page.pages, 3);
  assert.equal(page.items.length, 1);
  assert.equal(page.items[0].items.length, 1);
  assert.equal(page.items[0].bytes, 4096);
});

test('2000 cache files become one Codex directory with exact IDs across hidden pages', () => {
  const files = Array.from({ length: 2000 }, (_, index) => item(`codex-${index}`, {
    appName: null,
    path: `/Users/example/Library/Caches/Codex/${index}.log`,
    name: `${index}.log`, files: 1, bytes: 1024 + index,
  }));
  const group = groupItems(files)[0];
  assert.equal(group.name, 'Codex');
  const roots = buildDirectoryTree(group.items);
  assert.equal(roots.length, 1);
  const directory = roots[0];
  assert.equal(directory.name, 'Codex');
  assert.equal(directory.path, '/Users/example/Library/Caches/Codex');
  assert.equal(directory.id, undefined);
  assert.equal(directory.children.length, 0);
  assert.equal(directory.files.length, 2000);
  assert.equal(directory.candidateCount, 2000);
  assert.equal(directory.fileCount, 2000);
  assert.equal(directory.bytes, 2000 * 1024 + 1999 * 2000 / 2);
  assert.equal(directory.bytes, group.bytes);
  assert.deepEqual(new Set(directory.itemIds), new Set(files.map((file) => file.id)));
  const original = new Map(files.map((file) => [file.id, file]));
  directory.items.forEach((file) => assert.strictEqual(file, original.get(file.id)));

  const selected = new Set(directory.itemIds);
  const firstPage = pageItems(directory.files, 1);
  assert.equal(firstPage.items.length, 100);
  assert.equal(firstPage.items.some((file) => file.id === 'codex-0'), false);
  assert.deepEqual(selectedState(directory, selected), { count: 2000, checked: true, indeterminate: false });
  selected.delete('codex-0');
  assert.deepEqual(selectedState(directory, selected), { count: 1999, checked: false, indeterminate: true });
  assert.equal(pageItems(directory.files, 20).items.some((file) => file.id === 'codex-0'), true);
  assert.equal(buildDirectoryTree([...files].reverse())[0].key, directory.key);
});

test('directory branches sort by candidate bytes and single chains preserve their real paths', () => {
  const base = '/Users/example/Library/Logs/Codex';
  const files = [
    item('one', { appName: null, category: 'logs', path: `${base}/one/two/a.log`, bytes: 100, files: 1 }),
    item('other', { appName: null, category: 'logs', path: `${base}/other/deep/b.log`, bytes: 900, files: 1 }),
  ];
  const root = buildDirectoryTree(files)[0];
  assert.equal(root.name, 'Codex');
  assert.equal(root.files.length, 0);
  assert.equal(root.children.length, 2);
  assert.deepEqual(root.children.map((node) => node.bytes), [900, 100]);
  const branch = root.children[1];
  assert.equal(branch.name, 'one / two');
  assert.equal(branch.path, `${base}/one/two`);
  assert.deepEqual(branch.paths, [`${base}/one`, `${base}/one/two`]);
  assert.deepEqual(branch.itemIds, ['one']);
  const filtered = buildDirectoryTree([files[0]])[0];
  assert.equal(filtered.name, 'Codex / one / two');
  assert.deepEqual(filtered.paths, [base, `${base}/one`, `${base}/one/two`]);
  assert.equal(filtered.key, root.key);
  assert.strictEqual(filtered.files[0], files[0]);
});

test('manual candidates stay separate and filtered folder selection affects matching IDs only', () => {
  const base = '/Users/example/Library/Caches/Codex';
  const shown = item('shown', { appName: null, path: `${base}/shown.log`, files: 1 });
  const hidden = item('hidden', { appName: null, path: `${base}/hidden.log`, files: 1 });
  const manual = item('manual', { appName: null, path: `${base}/state.db`, selectedByDefault: false, files: 1 });
  const other = item('other', { appName: null, path: '/Users/example/Library/Caches/Codex-old/other.log', files: 1 });
  const groups = groupItems([shown, hidden, manual, other]);
  assert.equal(groups.length, 3);
  const suggested = groups.find((group) => group.name === 'Codex' && group.suggested);
  assert.deepEqual(new Set(suggested.items.map((file) => file.id)), new Set(['shown', 'hidden']));
  const mixedRoots = buildDirectoryTree([shown, manual]);
  assert.equal(mixedRoots.length, 2);
  assert.notEqual(mixedRoots[0].key, mixedRoots[1].key);
  assert.equal(mixedRoots.every((node) => node.candidateCount === 1), true);

  const selected = new Set(['shown', 'hidden', 'manual', 'other']);
  const filtered = buildDirectoryTree([shown])[0];
  filtered.items.forEach((file) => selected.delete(file.id));
  assert.deepEqual(selected, new Set(['hidden', 'manual', 'other']));
  assert.equal(manual.selectedByDefault, false);
});

test('original directory candidates retain their IDs without double-counting covered descendants', () => {
  const base = '/Users/example/Library/Caches/Codex';
  const parent = item('parent', { path: base, name: 'Codex', bytes: 400, files: 3 });
  const first = item('first', { path: `${base}/branch/first.log`, bytes: 100, files: 1 });
  const second = item('second', { path: `${base}/branch/second.log`, bytes: 200, files: 1 });
  const group = groupItems([parent, first, second])[0];
  const root = buildDirectoryTree(group.items)[0];
  assert.equal(group.bytes, 400);
  assert.equal(root.bytes, 400);
  assert.equal(root.fileCount, 3);
  assert.equal(root.candidateCount, 3);
  assert.deepEqual(new Set(root.itemIds), new Set(['parent', 'first', 'second']));
  assert.strictEqual(root.files[0], parent);
  assert.equal(root.children[0].bytes, 300);
  assert.equal(root.children[0].fileCount, 2);
  assert.deepEqual(selectedState(root, new Set(root.itemIds)), { count: 3, checked: true, indeterminate: false });

  const directoryOnly = buildDirectoryTree([parent])[0];
  assert.equal(directoryOnly.candidateCount, 1);
  assert.equal(directoryOnly.fileCount, 3);
  assert.deepEqual(directoryOnly.files, [parent]);
  assert.equal(directoryOnly.children.length, 0);
});

test('similar prefixes and separate user roots never merge their candidate IDs', () => {
  const files = [
    item('codex', { path: '/Users/one/Library/Caches/Codex/a.log', files: 1 }),
    item('old', { path: '/Users/one/Library/Caches/Codex-old/b.log', files: 1 }),
    item('other-root', { path: '/System/Volumes/Data/Users/two/Library/Caches/Codex/c.log', files: 1 }),
  ];
  const roots = buildDirectoryTree(files);
  assert.equal(roots.length, 3);
  assert.equal(new Set(roots.map((node) => node.key)).size, 3);
  const codex = roots.find((node) => node.path === '/Users/one/Library/Caches/Codex');
  assert.deepEqual(codex.itemIds, ['codex']);
  const selected = new Set();
  codex.items.forEach((file) => selected.add(file.id));
  assert.deepEqual(selected, new Set(['codex']));
  assert.equal(roots.reduce((bytes, node) => bytes + node.bytes, 0), groupItems(files)[0].bytes);
});

test('npm Cargo and sandbox cache branches share their exact cache roots without losing IDs or bytes', () => {
  const cases = [
    {
      root: '/Users/example/.npm/_cacache',
      files: ['content-v2/sha512/01/first', 'content-v2/sha512/ff/second', 'index-v5/ab/third'],
    },
    {
      root: '/Users/example/.cargo/registry/cache',
      files: ['index.crates.io-aaaa/first.crate', 'index.crates.io-bbbb/second.crate'],
    },
    {
      root: '/Users/example/Library/Containers/com.fixture.app/Data/Library/Caches',
      files: ['downloads/first.cache', 'compiled/second.cache'],
    },
  ];
  const allItems = [];
  for (const [source, fixture] of cases.entries()) {
    const files = fixture.files.map((relative, index) => item(`source-${source}-${index}`, {
      appName: null, path: `${fixture.root}/${relative}`, files: 1,
      bytes: (source + 1) * 100 + index, selectedByDefault: false, risk: 'review',
    }));
    allItems.push(...files);
    const groups = groupItems(files);
    assert.equal(groups.length, 1);
    const roots = buildDirectoryTree(files);
    assert.equal(roots.length, 1);
    const root = roots[0];
    assert.equal(root.path, fixture.root);
    assert.equal(root.paths[0], fixture.root);
    assert.equal(root.candidateCount, files.length);
    assert.equal(root.fileCount, files.length);
    assert.equal(root.bytes, files.reduce((bytes, file) => bytes + file.bytes, 0));
    assert.equal(root.bytes, groups[0].bytes);
    assert.deepEqual(new Set(root.itemIds), new Set(files.map((file) => file.id)));
    root.items.forEach((file) => assert.strictEqual(file, files.find((original) => original.id === file.id)));
  }
  const roots = buildDirectoryTree(allItems);
  assert.equal(roots.length, 3);
  assert.deepEqual(new Set(roots.map((root) => root.path)), new Set(cases.map((fixture) => fixture.root)));
  assert.deepEqual(new Set(roots.flatMap((root) => root.itemIds)), new Set(allItems.map((file) => file.id)));
  assert.equal(roots.reduce((bytes, root) => bytes + root.bytes, 0), allItems.reduce((bytes, file) => bytes + file.bytes, 0));
});
