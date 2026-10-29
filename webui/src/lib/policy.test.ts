import { describe, it, expect } from 'vitest';
import { verificationIsInsecure, verificationLabel } from '@/lib/policy';
import type { ProxyPolicy } from '@/types/api';

function withVerification(
  verification: NonNullable<ProxyPolicy['upstream_tls']>['verification'],
): Pick<ProxyPolicy, 'upstream_tls'> {
  return { upstream_tls: { verification } };
}

describe('target verification labels', () => {
  it('shows the default when the engine predates the field', () => {
    expect(verificationLabel({})).toBe('native roots');
    expect(verificationIsInsecure({})).toBe(false);
  });

  it('names a custom bundle by its path', () => {
    const policy = withVerification({ mode: 'custom_ca', path: '/etc/relaycore/internal.pem' });
    expect(verificationLabel(policy)).toBe('custom CA (/etc/relaycore/internal.pem)');
    expect(verificationIsInsecure(policy)).toBe(false);
  });

  it('marks disabled verification as DISABLED and as insecure', () => {
    const policy = withVerification({ mode: 'insecure', reason: 'lab fixture' });
    expect(verificationLabel(policy)).toBe('DISABLED');
    expect(verificationIsInsecure(policy)).toBe(true);
  });
});
