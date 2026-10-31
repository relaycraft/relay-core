import { describe, it, expect } from 'vitest';
import { toPem } from '@/lib/certificate';

describe('toPem', () => {
  it('wraps base64 at 64 characters with PEM guards', () => {
    const pem = toPem('A'.repeat(130));
    const lines = pem.split('\n');
    expect(lines[0]).toBe('-----BEGIN CERTIFICATE-----');
    expect(lines.at(-1)).toBe('-----END CERTIFICATE-----');
    expect(lines.slice(1, -1).map((line) => line.length)).toEqual([64, 64, 2]);
  });

  it('tolerates whitespace in the stored base64', () => {
    expect(toPem(' AA\nBB ')).toBe(
      '-----BEGIN CERTIFICATE-----\nAABB\n-----END CERTIFICATE-----',
    );
  });

  it('returns an empty string rather than an empty certificate', () => {
    expect(toPem('')).toBe('');
    expect(toPem('   ')).toBe('');
  });
});
