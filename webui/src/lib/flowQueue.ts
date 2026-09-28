import type { Flow } from '@/types/api';

/** How many captures one animation frame applies, so a burst cannot block the interface. */
export const FLOWS_PER_FRAME = 100;

/**
 * How many captures may wait for a frame before the backlog is treated as a gap.
 *
 * A frame callback does not run while the tab is in the background, so a proxy producing traffic
 * overnight queues every one of them — each a whole `Flow`, bodies included. The queue is the only
 * place those captures live, and nothing bounded it, so the tab grew until the browser killed it.
 * Past this many, the oldest are dropped and the list is re-synced from the API instead: one request
 * covers any number of missed snapshots, and the newest are the ones the reader is looking at.
 */
export const MAX_QUEUED_FLOWS = 1000;

export interface FlowBatcherOptions {
  /** Apply one capture to the list. */
  apply: (flow: Flow) => void;
  /** Ask for a full list refresh, after captures were dropped. */
  resync: () => void;
  maxPerFrame?: number;
  maxQueued?: number;
  schedule?: (run: () => void) => number;
  cancel?: (handle: number) => void;
}

export interface FlowBatcher {
  push(flow: Flow): void;
  /** Apply everything now, ignoring the frame boundary. */
  flush(): void;
  /** Drop what is queued and stop waiting for a frame. */
  reset(): void;
  /** How many captures are waiting. */
  pending(): number;
}

/**
 * Apply captures to the list one frame at a time.
 *
 * Extracted from the app root because the interesting part is not the batching but what happens when
 * the batching cannot keep up, and that only shows up under a hidden tab or a traffic burst — the two
 * conditions least likely to be present when someone is watching the interface.
 */
export function createFlowBatcher(options: FlowBatcherOptions): FlowBatcher {
  const maxPerFrame = options.maxPerFrame ?? FLOWS_PER_FRAME;
  const maxQueued = options.maxQueued ?? MAX_QUEUED_FLOWS;
  const schedule = options.schedule ?? ((run) => requestAnimationFrame(run));
  const cancel = options.cancel ?? ((handle) => cancelAnimationFrame(handle));

  let queue: Flow[] = [];
  let scheduled: number | null = null;
  let resyncPending = false;

  function flush() {
    scheduled = null;
    const batch = queue.splice(0, maxPerFrame);
    for (const flow of batch) options.apply(flow);
    // After the captures that survived: they are the newest, and the refresh merges its page into the
    // list rather than replacing it.
    if (resyncPending) {
      resyncPending = false;
      options.resync();
    }
    if (queue.length > 0) {
      scheduled = schedule(flush);
    }
  }

  return {
    push(flow: Flow) {
      const overflow = queue.length - maxQueued + 1;
      if (overflow > 0) {
        queue.splice(0, overflow);
        resyncPending = true;
      }
      queue.push(flow);
      if (scheduled === null) {
        scheduled = schedule(flush);
      }
    },
    flush,
    reset() {
      queue = [];
      resyncPending = false;
      if (scheduled !== null) {
        cancel(scheduled);
        scheduled = null;
      }
    },
    pending: () => queue.length,
  };
}
