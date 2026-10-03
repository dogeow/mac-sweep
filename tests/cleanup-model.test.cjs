const test = require('node:test');
const assert = require('node:assert/strict');
const { groupItems, selectedState, pageItems } = require('../frontend/cleanup-model.js');

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
