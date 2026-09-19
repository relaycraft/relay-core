import { describe, expect, it } from 'vitest';
import { COOKIE_NAME, initAuth, tokenFromFragment } from './auth';

function fakeWindow(hash: string) {
  const replaced: string[] = [];
  return {
    replaced,
    win: {
      location: { hash, pathname: '/', search: '' },
      history: {
        replaceState: (_state: unknown, _title: string, url: string) => {
          replaced.push(url);
        },
      },
    } as unknown as Window,
  };
}

function fakeDocument() {
  const store: { cookie: string } = { cookie: '' };
  return store as unknown as Document;
}

describe('tokenFromFragment', () => {
  it('reads the token the daemon printed', () => {
    expect(tokenFromFragment('#token=abc123')).toBe('abc123');
  });

  it('ignores a fragment without a token', () => {
    expect(tokenFromFragment('')).toBeNull();
    expect(tokenFromFragment('#flows')).toBeNull();
    expect(tokenFromFragment('#token=')).toBeNull();
  });
});

describe('initAuth', () => {
  it('trades the fragment for a cookie and strips the address bar', () => {
    const { win, replaced } = fakeWindow('#token=s3cret');
    const document_ = fakeDocument();

    expect(initAuth(win, document_)).toBe(true);

    expect(document_.cookie).toContain(`${COOKIE_NAME}=s3cret`);
    expect(document_.cookie).toContain('SameSite=Strict');
    expect(replaced).toEqual(['/']),
      'the token must not stay in the URL it was read from';
  });

  it('url-encodes the token so exotic values survive the round trip', () => {
    const { win } = fakeWindow('#token=a%2Fb%2Bc');
    const document_ = fakeDocument();

    initAuth(win, document_);

    expect(document_.cookie).toContain(`${COOKIE_NAME}=a%2Fb%2Bc`);
  });

  it('does nothing when the page was opened without a token', () => {
    const { win, replaced } = fakeWindow('');
    const document_ = fakeDocument();

    expect(initAuth(win, document_)).toBe(false);
    expect(document_.cookie).toBe('');
    expect(replaced).toEqual([]);
  });
});
