import type { JSX } from 'solid-js';

/**
 * Interface icons.
 *
 * These replace emoji glyphs, which render differently on every platform, cannot inherit the theme's
 * colour or stroke weight, and read as decoration rather than as interface. Each icon is a stroked
 * path using `currentColor`, so it follows the text colour it sits in — including in the light theme,
 * where an emoji's baked-in colours would clash.
 */
interface IconProps {
  /** Rendered size in pixels. Defaults to 16, the size of the labels these sit next to. */
  size?: number;
  class?: string;
}

function base(props: IconProps): {
  width: number;
  height: number;
  viewBox: string;
  fill: string;
  stroke: string;
  'stroke-width': number;
  'stroke-linecap': 'round';
  'stroke-linejoin': 'round';
  class: string;
  'aria-hidden': 'true';
} {
  return {
    width: props.size ?? 16,
    height: props.size ?? 16,
    viewBox: '0 0 24 24',
    fill: 'none',
    stroke: 'currentColor',
    'stroke-width': 2,
    'stroke-linecap': 'round',
    'stroke-linejoin': 'round',
    class: props.class ?? '',
    'aria-hidden': 'true',
  };
}

/** Traffic flows: a pulse on a line. */
export function IconFlows(props: IconProps): JSX.Element {
  return (
    <svg {...base(props)}>
      <path d="M3 12h4l3-7 4 14 3-7h4" />
    </svg>
  );
}

/** Interception workshop: a paused exchange. */
export function IconWorkshop(props: IconProps): JSX.Element {
  return (
    <svg {...base(props)}>
      <rect x="6" y="5" width="4" height="14" rx="1" />
      <rect x="14" y="5" width="4" height="14" rx="1" />
    </svg>
  );
}

/** Rules: an ordered list. */
export function IconRules(props: IconProps): JSX.Element {
  return (
    <svg {...base(props)}>
      <path d="M4 6h10M4 12h16M4 18h7" />
    </svg>
  );
}

/** Scripts: a bolt. */
export function IconScripts(props: IconProps): JSX.Element {
  return (
    <svg {...base(props)}>
      <path d="M13 2 4 14h6l-1 8 9-12h-6l1-8Z" />
    </svg>
  );
}

/** Settings: a gear. */
export function IconSettings(props: IconProps): JSX.Element {
  return (
    <svg {...base(props)}>
      <circle cx="12" cy="12" r="3" />
      <path d="M19.4 15a1.7 1.7 0 0 0 .3 1.9l.1.1a2 2 0 1 1-2.8 2.8l-.1-.1a1.7 1.7 0 0 0-2.9 1.2V21a2 2 0 1 1-4 0v-.1A1.7 1.7 0 0 0 7 19.4a1.7 1.7 0 0 0-1.9.3l-.1.1a2 2 0 1 1-2.8-2.8l.1-.1A1.7 1.7 0 0 0 3 14a1.7 1.7 0 0 0-1.6-1H1a2 2 0 1 1 0-4h.1A1.7 1.7 0 0 0 3 7.6a1.7 1.7 0 0 0-.3-1.9l-.1-.1a2 2 0 1 1 2.8-2.8l.1.1A1.7 1.7 0 0 0 8 3h.1A1.7 1.7 0 0 0 10 1.4V1a2 2 0 1 1 4 0v.1A1.7 1.7 0 0 0 16 3a1.7 1.7 0 0 0 1.9-.3l.1-.1a2 2 0 1 1 2.8 2.8l-.1.1A1.7 1.7 0 0 0 21 8v.1a1.7 1.7 0 0 0 1.6 1.3H23a2 2 0 1 1 0 4h-.1a1.7 1.7 0 0 0-1.5 1.6Z" transform="translate(0 1) scale(0.92)" />
    </svg>
  );
}

/** Warning: a triangle, used for states that need attention. */
export function IconWarning(props: IconProps): JSX.Element {
  return (
    <svg {...base(props)}>
      <path d="M10.3 3.9 1.8 18a2 2 0 0 0 1.7 3h17a2 2 0 0 0 1.7-3L13.7 3.9a2 2 0 0 0-3.4 0Z" />
      <path d="M12 9v4M12 17h.01" />
    </svg>
  );
}

/** Help: a question mark in a circle, so the last button in the bar is not a bare glyph. */
export function IconHelp(props: IconProps): JSX.Element {
  return (
    <svg {...base(props)}>
      <circle cx="12" cy="12" r="9" />
      <path d="M9.5 9a2.5 2.5 0 1 1 3.4 2.3c-.6.3-.9.8-.9 1.4v.3M12 17h.01" />
    </svg>
  );
}

/** Light or dark theme, depending on which one is currently active. */
export function IconTheme(props: IconProps): JSX.Element {
  return (
    <svg {...base(props)}>
      <circle cx="12" cy="12" r="4" />
      <path d="M12 2v2M12 20v2M4.9 4.9l1.4 1.4M17.7 17.7l1.4 1.4M2 12h2M20 12h2M4.9 19.1l1.4-1.4M17.7 6.3l1.4-1.4" />
    </svg>
  );
}
