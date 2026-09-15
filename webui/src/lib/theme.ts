/**
 * Theme selection.
 *
 * The stylesheet follows `prefers-color-scheme` on its own, so a user whose system is light already
 * gets the light theme without doing anything. This adds an explicit choice on top, because "follow
 * the system" is not the same as "let me pick", and someone reading traffic at night may want the
 * dark UI on a light desktop (or the reverse).
 *
 * The choice is stored as `data-theme` on the root element, which is also what the CSS keys off, so
 * there is one source of truth rather than a stored value and a rendered value that can disagree.
 */
export type Theme = 'light' | 'dark';

const STORAGE_KEY = 'relay-core.theme';

/** The stored choice, or null when the user has never chosen and the system decides. */
export function storedTheme(): Theme | null {
  try {
    const value = localStorage.getItem(STORAGE_KEY);
    return value === 'light' || value === 'dark' ? value : null;
  } catch {
    // Storage can be unavailable; falling back to the system preference is the right failure.
    return null;
  }
}

function apply(theme: Theme | null): void {
  const root = document.documentElement;
  if (theme === null) {
    root.removeAttribute('data-theme');
  } else {
    root.setAttribute('data-theme', theme);
  }
}

/** Apply the stored choice at startup, before the first paint. */
export function initTheme(): void {
  apply(storedTheme());
}

/** Which theme is on screen right now, taking the system preference into account. */
export function effectiveTheme(): Theme {
  const stored = storedTheme();
  if (stored) return stored;
  return window.matchMedia?.('(prefers-color-scheme: light)').matches ? 'light' : 'dark';
}

/** Choose a theme and remember it. */
export function setTheme(theme: Theme): void {
  try {
    localStorage.setItem(STORAGE_KEY, theme);
  } catch {
    // Still apply it for this session even if it cannot be remembered.
  }
  apply(theme);
}
