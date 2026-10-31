import { describe, it, expect } from 'vitest';
import { describeRuleEvaluation } from '@/lib/ruleTrace';

describe('describeRuleEvaluation', () => {
  it('says a rule did not match without repeating a redundant verdict', () => {
    expect(
      describeRuleEvaluation({
        rule_id: 'post-only',
        stage: 'RequestHeaders',
        matched: false,
        duration_us: 4,
        outcome: 'NotMatched',
      }),
    ).toBe('post-only did not match at RequestHeaders');
  });

  it('names the failure reason', () => {
    expect(
      describeRuleEvaluation({
        rule_id: 'rewrite',
        stage: 'ResponseBody',
        matched: true,
        duration_us: 40,
        outcome: { Failed: 'body budget exceeded' },
      }),
    ).toBe('rewrite matched at ResponseBody (failed: body budget exceeded, 40µs)');
  });

  it('renders the other outcomes as readable words', () => {
    expect(
      describeRuleEvaluation({
        rule_id: 'skip',
        stage: 'RequestBody',
        matched: false,
        duration_us: 1,
        outcome: 'Skipped',
      }),
    ).toBe('skip did not match at RequestBody (skipped, 1µs)');
  });
});
