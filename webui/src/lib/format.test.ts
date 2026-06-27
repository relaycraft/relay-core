import { describe, expect, it } from 'vitest';
import { formatDurationMs, formatFlowTime } from './format';

describe('format', () => {
  it('formatFlowTime returns a time string', () => {
    const text = formatFlowTime(Date.UTC(2024, 5, 15, 14, 30, 5));
    expect(text.length).toBeGreaterThan(0);
    expect(text).toMatch(/\d/);
  });

  it('formatDurationMs uses locale number formatting', () => {
    expect(formatDurationMs(42)).toContain('42');
    expect(formatDurationMs(42)).toContain('ms');
  });
});
