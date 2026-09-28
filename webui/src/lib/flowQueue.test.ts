import { describe, expect, it } from 'vitest';
import { createFlowBatcher, FLOWS_PER_FRAME } from './flowQueue';
import type { Flow } from '@/types/api';

function flow(id: string): Flow {
  return { id } as unknown as Flow;
}

/** A scheduler the test drives by hand, so "the next frame" is an explicit step. */
function manualScheduler() {
  const frames: (() => void)[] = [];
  const cancelled: number[] = [];
  let next = 1;
  return {
    schedule: (run: () => void) => {
      frames.push(run);
      return next++;
    },
    cancel: (handle: number) => {
      cancelled.push(handle);
    },
    /** Run one frame. */
    frame: () => {
      const run = frames.shift();
      run?.();
    },
    pendingFrames: () => frames.length,
    cancelled,
  };
}

function batcher(maxPerFrame = 2, maxQueued = 5) {
  const applied: string[] = [];
  const sched = manualScheduler();
  let resyncs = 0;
  const batcher = createFlowBatcher({
    apply: (f) => applied.push(f.id),
    resync: () => {
      resyncs += 1;
    },
    maxPerFrame,
    maxQueued,
    schedule: sched.schedule,
    cancel: sched.cancel,
  });
  return { batcher, applied, sched, resyncs: () => resyncs };
}

describe('createFlowBatcher', () => {
  it('applies at most a frame worth per frame, in arrival order', () => {
    const { batcher: b, applied, sched } = batcher();
    for (const id of ['a', 'b', 'c', 'd', 'e']) b.push(flow(id));

    expect(b.pending()).toBe(5);
    expect(sched.pendingFrames()).toBe(1);
    sched.frame();
    expect(applied).toEqual(['a', 'b']);
    sched.frame();
    expect(applied).toEqual(['a', 'b', 'c', 'd']);
    sched.frame();
    expect(applied).toEqual(['a', 'b', 'c', 'd', 'e']);
    expect(b.pending()).toBe(0);
  });

  it('schedules nothing more once the queue drains', () => {
    const { batcher: b, sched } = batcher();
    b.push(flow('a'));
    sched.frame();
    expect(sched.pendingFrames()).toBe(0);

    // A later push schedules again: the batcher must not think a frame is still pending.
    b.push(flow('b'));
    expect(sched.pendingFrames()).toBe(1);
  });

  it('drops the oldest captures past the cap and asks for one resync', () => {
    const { batcher: b, applied, sched, resyncs } = batcher(1, 3);
    for (const id of ['a', 'b', 'c', 'd', 'e']) b.push(flow(id));

    // `a` and `b` were dropped to stay within three queued; the newest three are kept.
    expect(b.pending()).toBe(3);
    sched.frame();
    expect(applied).toEqual(['c']);
    expect(resyncs()).toBe(1);
    sched.frame();
    sched.frame();
    expect(applied).toEqual(['c', 'd', 'e']);
  });

  it('resyncs once for a whole backlog, not once per dropped capture', () => {
    const { batcher: b, sched, resyncs } = batcher(10, 3);
    for (let i = 0; i < 20; i++) b.push(flow(`f${i}`));
    sched.frame();
    expect(resyncs()).toBe(1);
    sched.frame();
    expect(resyncs()).toBe(1);
  });

  it('applies the surviving captures before the resync', () => {
    const order: string[] = [];
    const sched = manualScheduler();
    const b = createFlowBatcher({
      apply: (f) => order.push(`apply:${f.id}`),
      resync: () => order.push('resync'),
      maxPerFrame: 10,
      maxQueued: 2,
      schedule: sched.schedule,
      cancel: sched.cancel,
    });
    b.push(flow('a'));
    b.push(flow('b'));
    b.push(flow('c'));
    sched.frame();
    expect(order).toEqual(['apply:b', 'apply:c', 'resync']);
  });

  it('reset stops the pending frame', () => {
    const { batcher: b, applied, sched } = batcher();
    b.push(flow('a'));
    b.reset();
    expect(b.pending()).toBe(0);
    expect(sched.cancelled).toEqual([1]);
    sched.frame();
    expect(applied).toEqual([]);
  });

  it('uses a frame-sized batch by default', () => {
    const applied: string[] = [];
    const sched = manualScheduler();
    const b = createFlowBatcher({
      apply: (f) => applied.push(f.id),
      resync: () => {},
      schedule: sched.schedule,
      cancel: sched.cancel,
    });
    for (let i = 0; i < FLOWS_PER_FRAME + 1; i++) b.push(flow(`f${i}`));
    sched.frame();
    expect(applied.length).toBe(FLOWS_PER_FRAME);
  });
});
