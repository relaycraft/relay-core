import { describe, expect, it } from 'vitest';
import { createRateMeter } from './rate';

describe('createRateMeter', () => {
  it('has no rate until it has two readings', () => {
    const meter = createRateMeter();
    // The counter is a lifetime total: 5000 requests at 10 s past the proxy's start is not a rate of
    // 500/s, and that is what reading it against an initial zero reported.
    expect(meter(5000, 10_000)).toBeNull();
    expect(meter(5010, 12_000)).toBe(5);
  });

  it('measures against the previous reading, not the first', () => {
    const meter = createRateMeter();
    meter(0, 0);
    expect(meter(100, 2_000)).toBe(50);
    expect(meter(200, 4_000)).toBe(50);
  });

  it('ignores readings that arrive inside the minimum interval', () => {
    const meter = createRateMeter(1);
    meter(0, 0);
    expect(meter(5, 200)).toBeNull();
    // The ignored reading must not become the baseline, or the next rate would cover a fifth of the
    // interval and read five times too high.
    expect(meter(100, 2_000)).toBe(50);
  });

  it('reports zero rather than a negative rate after a restart', () => {
    const meter = createRateMeter();
    meter(1000, 0);
    expect(meter(10, 2_000)).toBe(0);
    expect(meter(30, 4_000)).toBe(10);
  });

  it('rounds to whole requests per second', () => {
    const meter = createRateMeter();
    meter(0, 0);
    expect(meter(5, 2_000)).toBe(3);
  });
});
