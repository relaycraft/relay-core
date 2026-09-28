/**
 * A counter's rate per second, from successive readings of that counter.
 *
 * The first sample cannot produce a rate: dividing a total by the time since a page was opened gives
 * a number that was never true — reading a lifetime total against a `prevTotal` of `0` reported the
 * counter's whole history as a per-second rate for one interval, then corrected itself. So the first
 * reading is a baseline and the answer is `null` until there are two.
 *
 * Readings closer together than `minSeconds` are ignored rather than averaged, which is what makes
 * the caller's polling interval the resolution of the number.
 */
export function createRateMeter(minSeconds = 1) {
  let previous: { total: number; at: number } | null = null;

  return function sample(total: number, at: number): number | null {
    if (previous === null) {
      previous = { total, at };
      return null;
    }
    const seconds = (at - previous.at) / 1000;
    if (seconds < minSeconds) return null;
    // A counter that went backwards means the engine restarted; the rate for that interval is not
    // negative, it is unmeasured, and the next reading is measured against the new total.
    const rate = (total - previous.total) / seconds;
    previous = { total, at };
    return Math.max(0, Math.round(rate));
  };
}
