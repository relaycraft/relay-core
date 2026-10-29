import type { ProxyPolicy } from '@/types/api';

/** How the target-certificate verification policy should read in the interface. */
export function verificationLabel(policy: Pick<ProxyPolicy, 'upstream_tls'>): string {
  const verification = policy.upstream_tls?.verification;
  if (!verification) return 'native roots';
  if (verification.mode === 'custom_ca') return `custom CA (${verification.path})`;
  if (verification.mode === 'insecure') return 'DISABLED';
  return 'native roots';
}

/**
 * Whether this engine accepts any certificate a target presents.
 *
 * Exposed separately from the label so the caller can style it as an error without parsing the
 * text: a security state must not depend on a string comparison.
 */
export function verificationIsInsecure(policy: Pick<ProxyPolicy, 'upstream_tls'>): boolean {
  return policy.upstream_tls?.verification.mode === 'insecure';
}
