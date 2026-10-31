/**
 * Render a base64 DER certificate as PEM.
 *
 * The engine records base64 DER because that is what rustls hands it: re-encoding on capture would
 * add work to every TLS connection for a representation only a reader needs. The wire format is
 * documented; this is where a person gets something they can paste into `openssl`.
 */
export function toPem(base64Der: string): string {
  const compact = base64Der.replace(/\s+/g, '');
  if (compact.length === 0) return '';
  const lines = compact.match(/.{1,64}/g) ?? [];
  return `-----BEGIN CERTIFICATE-----\n${lines.join('\n')}\n-----END CERTIFICATE-----`;
}
