(function (root) {
  'use strict';
  // Percent estimates describe traversal work, not bytes. A ticking clock may
  // decrease an ETA, but must never fabricate scan progress or completion.
  function create() {
    let started = 0, points = [], deadline = null, lastAdvance = null, finished = false;
    function reset(now) {
      started = now;
      points = [];
      deadline = lastAdvance = null;
      finished = false;
    }
    function observe(percent, now) {
      if (finished || !Number.isFinite(now) || now < started) return;
      if (percent === null) { points = []; deadline = lastAdvance = null; return; }
      if (!Number.isFinite(percent) || percent < 0 || percent > 99) return;
      const previous = points.at(-1);
      if (previous && (now < previous.time || percent < previous.percent)) {
        points = []; deadline = lastAdvance = null;
      }
      const last = points.at(-1);
      // Repeated identical readings do not keep an old ETA artificially alive.
      if (last && percent <= last.percent + .001) return;
      lastAdvance = now;
      points.push({ time: now, percent });
      points = points.filter((point) => now - point.time <= 30_000).slice(-150);
      const elapsed = now - started;
      if (elapsed < 3000 || percent < 1 || percent >= 99) { deadline = null; return; }
      const overallRate = percent / elapsed;
      const first = points[0];
      const span = now - first.time;
      // Bound the effect of short fast/slow branches; an individual directory
      // completion can jump the estimate without representing sustained speed.
      const recentRate = span >= 4000 && percent - first.percent >= .1
        ? Math.max(overallRate * .5, Math.min(overallRate * 2, (percent - first.percent) / span))
        : overallRate;
      const remaining = (100 - percent) / (overallRate * .7 + recentRate * .3);
      if (!Number.isFinite(remaining) || remaining > 24 * 60 * 60 * 1000) { deadline = null; return; }
      const predicted = now + remaining;
      if (deadline === null || !last || now - last.time > 3000) deadline = predicted;
      else {
        const alpha = 1 - Math.exp(-Math.max(0, now - last.time) / 3000);
        deadline += (predicted - deadline) * alpha;
      }
    }
    function snapshot(now, waiting = false) {
      if (finished) return { kind: 'complete' };
      const percent = points.at(-1)?.percent;
      if (waiting) return { kind: 'waiting' };
      if (percent >= 99) return { kind: 'finishing' };
      if (lastAdvance !== null && now - lastAdvance >= 8000) return { kind: 'recalculating' };
      if (deadline === null) return { kind: 'preparing' };
      // An estimate expiring is not proof of completion. Wait for a new work
      // reading, rather than sticking at a false zero or auto-restarting a timer.
      if (now >= deadline) return { kind: 'recalculating' };
      return { kind: 'countdown', seconds: Math.max(1, Math.ceil((deadline - now) / 1000)) };
    }
    return { reset, observe, snapshot, finish: () => { finished = true; deadline = null; } };
  }
  function format(value) {
    if (value.kind === 'countdown') {
      const seconds = value.seconds;
      const hours = Math.floor(seconds / 3600);
      const minutes = Math.floor(seconds / 60) % 60;
      const clock = `${String(minutes).padStart(2, '0')}:${String(seconds % 60).padStart(2, '0')}`;
      return `预计剩余 ${hours ? `${hours}:${clock}` : clock}`;
    }
    return ({ preparing: '正在估算剩余时间…', recalculating: '正在重新估算剩余时间…', waiting: '等待系统返回，剩余时间更新中…', finishing: '正在完成统计…', complete: '本轮分析已完成' })[value.kind] || '';
  }
  const api = { create, format };
  if (typeof module === 'object' && module.exports) module.exports = api;
  else root.macSweepAnalysisTiming = api;
})(typeof window === 'object' ? window : globalThis);
