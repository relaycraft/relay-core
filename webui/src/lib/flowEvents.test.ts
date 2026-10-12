import { describe, it, expect } from 'vitest';
import {
  applyNotice,
  createFlowEventState,
  describeFlowEvent,
  MAX_NOTICES_PER_FLOW,
  type FlowEventNotice,
} from '@/lib/flowEvents';

const FLOW = '11111111-1111-1111-1111-111111111111';

describe('describeFlowEvent', () => {
  it('names who changed what, which is the reason the variant exists', () => {
    const notice = describeFlowEvent({
      event: 'mutation_applied',
      flow_id: FLOW,
      direction: 'ClientToServer',
      actor: 'rule:add-auth',
      fields: ['request.headers.authorization'],
    });

    expect(notice?.kind).toBe('mutation_applied');
    expect(notice?.flowId).toBe(FLOW);
    expect(notice?.summary).toBe('rule:add-auth changed request.headers.authorization');
  });

  it('reports a pause with its phase', () => {
    const notice = describeFlowEvent({
      event: 'intercept_paused',
      flow_id: FLOW,
      phase: 'response_body',
    });
    expect(notice?.summary).toBe('paused at response_body');
  });

  it('distinguishes a resume with edits from a plain resume', () => {
    const plain = describeFlowEvent({
      event: 'intercept_resolved',
      flow_id: FLOW,
      phase: 'request_headers',
      mutated: false,
    });
    const edited = describeFlowEvent({
      event: 'intercept_resolved',
      flow_id: FLOW,
      phase: 'request_headers',
      mutated: true,
    });
    expect(plain?.summary).toBe('resumed at request_headers');
    expect(edited?.summary).toBe('resumed at request_headers with edits');
  });

  it('reads a body chunk the way the engine serialises it', () => {
    const notice = describeFlowEvent({
      event: 'body_chunk',
      flow_id: FLOW,
      direction: 'ServerToClient',
      observed_bytes: 4096,
      truncated: true,
    });
    expect(notice?.summary).toBe('body chunk, 4096 bytes observed (truncated)');
  });

  it('reads the nested failure reason the CloseReason tag produces', () => {
    const notice = describeFlowEvent({
      event: 'errored',
      flow_id: FLOW,
      reason: { reason: 'timeout', kind: 'idle' },
    });
    expect(notice?.kind).toBe('errored');
    expect(notice?.summary).toBe('failed (timeout)');
  });

  it('ignores an event from a newer engine instead of guessing or throwing', () => {
    // A UI that invented a meaning for an unknown variant would be the Trace-tab freeze again; one
    // that threw would take the whole update queue with it.
    expect(describeFlowEvent({ event: 'quic_stream_migrated', flow_id: FLOW })).toBeNull();
    expect(describeFlowEvent({ event: 'started' })).toBeNull();
    expect(describeFlowEvent(null)).toBeNull();
    expect(describeFlowEvent('nonsense')).toBeNull();
    expect(describeFlowEvent({ flow_id: FLOW })).toBeNull();
  });
});

describe('applyNotice', () => {
  function notice(kind: FlowEventNotice['kind'], flowId = FLOW): FlowEventNotice {
    return { flowId, kind, summary: kind, receivedAt: 0 };
  }

  it('tracks which flows are waiting on a breakpoint', () => {
    const state = createFlowEventState();
    applyNotice(state, notice('intercept_paused'));
    expect(state.paused).toEqual([FLOW]);

    // Pausing twice must not count twice: the count is of flows waiting, not of events seen.
    applyNotice(state, notice('intercept_paused'));
    expect(state.paused).toEqual([FLOW]);

    applyNotice(state, notice('intercept_resolved'));
    expect(state.paused).toEqual([]);
  });

  it('stops waiting when the flow ends, even without a resolve', () => {
    const state = createFlowEventState();
    applyNotice(state, notice('intercept_paused'));
    applyNotice(state, notice('completed'));
    expect(
      state.paused,
      'a flow that ended is not waiting on anything; leaving it paused would show a breakpoint that cannot be answered',
    ).toEqual([]);
  });

  it('keeps notices per flow, bounded', () => {
    const state = createFlowEventState();
    for (let i = 0; i < MAX_NOTICES_PER_FLOW + 10; i++) {
      applyNotice(state, notice('body_chunk'));
    }
    const kept = state.notices.get(FLOW) ?? [];
    expect(kept.length).toBe(MAX_NOTICES_PER_FLOW);
  });

  it('keeps different flows apart', () => {
    const state = createFlowEventState();
    applyNotice(state, notice('intercept_paused', 'flow-a'));
    applyNotice(state, notice('intercept_paused', 'flow-b'));
    applyNotice(state, notice('intercept_resolved', 'flow-a'));

    expect(state.paused).toEqual(['flow-b']);
    expect(state.notices.get('flow-a')?.length).toBe(2);
  });
});
