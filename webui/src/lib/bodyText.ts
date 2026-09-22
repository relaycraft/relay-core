import type { BodyData } from '@/types/api';

/**
 * Turn a captured body into the text a person reads.
 *
 * Text that is not UTF-8 is stored as base64 (the engine only keeps a string when the bytes are
 * valid UTF-8). Decoding those bytes as UTF-8 is what made GBK and GB2312 pages look garbled:
 * the charset is on the Content-Type, or in a `<meta charset>`, and has to be used.
 */
export function decodeBodyText(body: BodyData, contentType: string): string {
  if (body.encoding !== 'base64') return body.content;
  const bytes = bodyBytes(body);
  return decodeBytes(bytes, charsetFor(contentType, bytes));
}

/** Raw bytes of a body, whether it was stored as text or as base64. */
export function bodyBytes(data: BodyData): Uint8Array {
  if (data.encoding === 'base64') {
    try {
      const bin = atob(data.content);
      return Uint8Array.from(bin, (c) => c.charCodeAt(0));
    } catch {
      return new TextEncoder().encode(data.content);
    }
  }
  return new TextEncoder().encode(data.content);
}

function charsetFor(contentType: string, bytes: Uint8Array): string {
  const declared = declaredCharset(contentType);
  if (declared) return declared;
  const type = contentType.toLowerCase();
  if (type.includes('html') || type === '') {
    return metaCharset(bytes) ?? 'utf-8';
  }
  return 'utf-8';
}

function declaredCharset(contentType: string): string | undefined {
  const match = /charset\s*=\s*["']?([a-zA-Z0-9._-]+)/i.exec(contentType);
  return match?.[1];
}

/** `<meta charset=gbk>` in the first kilobyte, read as bytes so a wrong decode cannot hide it. */
function metaCharset(bytes: Uint8Array): string | undefined {
  const head = bytes.subarray(0, 1024);
  let ascii = '';
  for (const byte of head) ascii += String.fromCharCode(byte);
  const match = /charset\s*=\s*["']?([a-zA-Z0-9._-]+)/i.exec(ascii);
  return match?.[1];
}

function decodeBytes(bytes: Uint8Array, charset: string): string {
  try {
    return new TextDecoder(normalizeCharset(charset)).decode(bytes);
  } catch {
    return new TextDecoder('utf-8').decode(bytes);
  }
}

function normalizeCharset(name: string): string {
  const normalized = name.trim().toLowerCase();
  if (normalized === 'utf8' || normalized === 'utf-8') return 'utf-8';
  if (
    normalized === 'gbk' ||
    normalized === 'gb2312' ||
    normalized === 'gb18030' ||
    normalized === 'x-gbk'
  ) {
    return 'gb18030';
  }
  if (normalized === 'big5' || normalized === 'big5-hkscs' || normalized === 'cn-big5') return 'big5';
  if (normalized === 'latin1' || normalized === 'iso-8859-1') return 'windows-1252';
  return normalized;
}
