/** Locale-aware formatters (browser default via `undefined` locale). */

const flowTimeFormatter = new Intl.DateTimeFormat(undefined, {
  hour: '2-digit',
  minute: '2-digit',
  second: '2-digit',
  hourCycle: 'h23',
});

const integerFormatter = new Intl.NumberFormat(undefined, {
  maximumFractionDigits: 0,
});

/** Flow list timestamp — local time, 24h where supported. */
export function formatFlowTime(epochMs: number): string {
  return flowTimeFormatter.format(new Date(epochMs));
}

/** Duration label for flow list (e.g. `42 ms` with locale grouping). */
export function formatDurationMs(ms: number): string {
  return `${integerFormatter.format(ms)} ms`;
}
