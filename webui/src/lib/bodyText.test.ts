import { describe, expect, it } from 'vitest';
import { decodeBodyText } from './bodyText';

/** GBK for 你好. */
const GBK_HELLO = [0xc4, 0xe3, 0xba, 0xc3];

function base64(bytes: number[]): string {
  return btoa(String.fromCharCode(...bytes));
}

describe('decodeBodyText', () => {
  it('returns a utf-8 body as stored', () => {
    expect(
      decodeBodyText(
        { encoding: 'utf-8', content: '你好', size: 6 },
        'text/plain; charset=utf-8',
      ),
    ).toBe('你好');
  });

  it('decodes a gbk body from the content-type charset', () => {
    expect(
      decodeBodyText(
        { encoding: 'base64', content: base64(GBK_HELLO), size: 4 },
        'text/html; charset=gbk',
      ),
    ).toBe('你好');
  });

  it('decodes gb2312 the same way', () => {
    expect(
      decodeBodyText(
        { encoding: 'base64', content: base64(GBK_HELLO), size: 4 },
        'text/html; charset=gb2312',
      ),
    ).toBe('你好');
  });

  it('reads a meta charset when the header does not name one', () => {
    const prefix = Array.from(new TextEncoder().encode('<meta charset=gbk>'));
    expect(
      decodeBodyText(
        { encoding: 'base64', content: base64([...prefix, ...GBK_HELLO]), size: prefix.length + 4 },
        'text/html',
      ),
    ).toContain('你好');
  });
});
