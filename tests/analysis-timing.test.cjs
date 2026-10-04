const test = require('node:test');
const assert = require('node:assert/strict');
const timing = require('../frontend/analysis-timing.js');

test('remaining time counts down between real progress updates without inventing progress', () => {
  const model = timing.create();
  model.reset(0);
  model.observe(10, 10_000);
  assert.deepEqual(model.snapshot(10_000), { kind: 'countdown', seconds: 90 });
  model.observe(20, 20_000);
  assert.deepEqual(model.snapshot(21_000), { kind: 'countdown', seconds: 79 });
  assert.equal(timing.format(model.snapshot(21_000)), '预计剩余 01:19');
  assert.equal(model.snapshot(28_000).kind, 'recalculating');
});

test('constant percentages expire rather than repeatedly restarting the countdown', () => {
  const model = timing.create();
  model.reset(0);
  model.observe(30, 10_000);
  for (let time = 11_000; time <= 30_000; time += 1000) model.observe(30, time);
  assert.equal(model.snapshot(30_000).kind, 'recalculating');
  model.observe(50, 31_000);
  assert.equal(model.snapshot(31_000).kind, 'countdown');
  assert.ok(model.snapshot(31_000).seconds > 0);
});

test('unknown work, warmup, waits and 99 percent cannot announce a zero-second finish', () => {
  const model = timing.create();
  model.reset(0);
  model.observe(5, 1000);
  assert.equal(model.snapshot(1000).kind, 'preparing');
  model.observe(30, 10_000);
  assert.equal(model.snapshot(10_000, true).kind, 'waiting');
  model.observe(null, 11_000);
  assert.equal(model.snapshot(11_000).kind, 'preparing');
  model.observe(99, 12_000);
  assert.equal(model.snapshot(12_000).kind, 'finishing');
  assert.equal(timing.format(model.snapshot(12_000)), '正在完成统计…');
  model.finish();
  assert.equal(model.snapshot(13_000).kind, 'complete');
  model.observe(10, 14_000);
  assert.equal(model.snapshot(14_000).kind, 'complete');
});

test('an expired deadline recalculates without false zero, including almost-complete scans', () => {
  const model = timing.create();
  model.reset(0);
  model.observe(98, 10_000);
  assert.equal(model.snapshot(10_000).seconds, 1);
  assert.equal(model.snapshot(11_000).kind, 'recalculating');
});

test('invalid events cannot poison a valid estimate and each new scan resets its lifetime', () => {
  const model = timing.create();
  model.reset(0);
  model.observe(20, 10_000);
  const valid = model.snapshot(10_000);
  for (const percent of [NaN, Infinity, -1, 100, '50', undefined]) model.observe(percent, 11_000);
  model.observe(30, NaN);
  model.observe(30, -1);
  assert.deepEqual(model.snapshot(10_000), valid);
  model.reset(50_000);
  assert.equal(model.snapshot(50_000).kind, 'preparing');
  model.observe(20, 51_000);
  assert.equal(model.snapshot(51_000).kind, 'preparing');
  model.observe(20.5, 55_000);
  assert.ok(model.snapshot(55_000).seconds < valid.seconds);
});

test('rate changes adjust the estimate while subsecond notifications retain a speed window', () => {
  const model = timing.create();
  model.reset(0);
  for (let time = 200; time <= 10_000; time += 200) model.observe(time / 1000, time);
  const first = model.snapshot(10_000).seconds;
  for (let time = 10_200; time <= 20_000; time += 200) model.observe(10 + (time - 10_000) / 2000, time);
  assert.ok(model.snapshot(20_000).seconds > first);
  assert.ok(model.snapshot(20_000).seconds < first * 2);
  model.observe(40, 25_000);
  assert.ok(model.snapshot(25_000).seconds < first);
});

test('clock formatting supports minutes and hours without changing the estimate', () => {
  assert.equal(timing.format({ kind: 'countdown', seconds: 59 }), '预计剩余 00:59');
  assert.equal(timing.format({ kind: 'countdown', seconds: 3661 }), '预计剩余 1:01:01');
  assert.match(timing.format({ kind: 'waiting' }), /等待系统返回/);
});
