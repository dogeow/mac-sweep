(function (root) {
  'use strict';
  // Percent estimates describe traversal work, not bytes. A ticking clock may
  // decrease an ETA, but must never fabricate scan progress or completion.
  function create() {
    let started = 0, points = [], deadline = null, lastAdvance = null, finished = false;
    let unknownSince = null, validSince = null, degraded = false;
    const GAP_GRACE = 10_000;
    const RECOVERY_WINDOW = 2000;
    function markDegraded() {
      if (!degraded) validSince = null;
      degraded = true;
    }
    function reset(now) {
      started = now;
      points = [];
      deadline = lastAdvance = null;
      finished = false;
      unknownSince = validSince = null;
      degraded = false;
    }
    function observe(percent, now, invalidated = false) {
      if (finished || !Number.isFinite(now) || now < started) return;
      if (invalidated) {
        points = []; deadline = lastAdvance = validSince = null;
        unknownSince = now;
        degraded = true;
        return;
      }
      if (percent === null) {
        if (unknownSince === null) unknownSince = now;
        validSince = null;
        // Directory boundaries briefly hide the work denominator. Keep the
        // bounded last prediction across this gap instead of flickering.
        return;
      }
      if (!Number.isFinite(percent) || percent < 0 || percent > 99) return;
      if ((unknownSince !== null && now - unknownSince >= GAP_GRACE)
        || (lastAdvance !== null && now - lastAdvance >= GAP_GRACE)
        || (deadline !== null && now >= deadline)) markDegraded();
      unknownSince = null;
      const previous = points.at(-1);
      if (previous && (now < previous.time || percent < previous.percent)) {
        points = []; deadline = lastAdvance = null;
        degraded = true;
        validSince = now;
      }
      const last = points.at(-1);
      // Repeated identical readings do not keep an old ETA artificially alive.
      if (last && percent <= last.percent + .001) {
        if (degraded && lastAdvance !== null && now - lastAdvance >= GAP_GRACE) validSince = null;
        return;
      }
      if (validSince === null) validSince = now;
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
      // 99 is a finishing state, never a zero-second timer or completion signal.
      if (percent >= 99 && unknownSince === null) return { kind: 'finishing' };
      const unknownTooLong = unknownSince !== null && now - unknownSince >= GAP_GRACE;
      const stalled = lastAdvance !== null && now - lastAdvance >= GAP_GRACE;
      if (unknownTooLong || stalled || (deadline !== null && now >= deadline)) markDegraded();
      if (degraded) {
        // A single healthy frame cannot flip a prolonged wait back to a clock.
        const recovered = !waiting && unknownSince === null && validSince !== null
          && now - validSince >= RECOVERY_WINDOW && !stalled && deadline !== null && deadline > now;
        if (recovered) degraded = false;
        else return { kind: 'recalculating' };
      }
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
    return ({ preparing: '正在估算剩余时间…', recalculating: '正在更新预计时间…', waiting: '正在更新预计时间…', finishing: '正在核对剩余内容…', complete: '本轮分析已完成' })[value.kind] || '';
  }
  const api = { create, format };
  if (typeof module === 'object' && module.exports) module.exports = api;
  else root.macSweepAnalysisTiming = api;
})(typeof window === 'object' ? window : globalThis);
