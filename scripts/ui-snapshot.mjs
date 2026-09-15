#!/usr/bin/env node
/**
 * Screenshot the Web UI by driving it, not by describing it.
 *
 * Reading the stylesheet cannot tell you that a column collides with the next one after the type
 * scale grows, or that a panel has no visual hierarchy — that only shows up on screen. This exists so
 * reviewing the UI is repeatable: every view, in both themes, captured on demand.
 *
 * It drives Chrome over the DevTools Protocol with no dependencies (Node has a built-in WebSocket),
 * and it **clicks** rather than rewriting the page: an earlier attempt to force the light theme by
 * writing `data-theme` into the HTML proved nothing, because the app's own init removes that
 * attribute on startup. Interacting with the UI is the only honest way to see what a user sees.
 *
 * Usage:
 *   node scripts/ui-snapshot.mjs <ui-url> <out-dir> [--dpr=1] [--settle=1500]
 *
 * Requires Chrome running with --remote-debugging-port=9222; scripts/ui-fixtures.sh starts it.
 */
import { writeFileSync, mkdirSync } from 'node:fs';
import { join } from 'node:path';

const [url, outDir] = process.argv.slice(2);
const dpr = Number((process.argv.find((a) => a.startsWith('--dpr=')) ?? '--dpr=1').split('=')[1]);
const settle = Number(
  (process.argv.find((a) => a.startsWith('--settle=')) ?? '--settle=1500').split('=')[1]
);

if (!url || !outDir) {
  console.error('usage: node scripts/ui-snapshot.mjs <ui-url> <out-dir> [--dpr=1] [--settle=1500]');
  process.exit(1);
}
mkdirSync(outDir, { recursive: true });

const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

/** A minimal CDP client: send a method, await its result, keep the session id in the envelope. */
class Cdp {
  constructor(ws) {
    this.ws = ws;
    this.id = 0;
    this.pending = new Map();
    ws.addEventListener('message', (event) => {
      const msg = JSON.parse(event.data);
      if (msg.id && this.pending.has(msg.id)) {
        const { resolve, reject } = this.pending.get(msg.id);
        this.pending.delete(msg.id);
        msg.error ? reject(new Error(`${msg.error.message} (${JSON.stringify(msg.error.data)})`)) : resolve(msg.result);
      }
    });
  }

  static async connect(wsUrl) {
    const ws = new WebSocket(wsUrl);
    await new Promise((resolve, reject) => {
      ws.addEventListener('open', resolve, { once: true });
      ws.addEventListener('error', reject, { once: true });
    });
    return new Cdp(ws);
  }

  send(method, params = {}, sessionId) {
    const id = ++this.id;
    const payload = { id, method, params };
    if (sessionId) payload.sessionId = sessionId;
    this.ws.send(JSON.stringify(payload));
    return new Promise((resolve, reject) => this.pending.set(id, { resolve, reject }));
  }
}

/** Find the browser endpoint that Chrome exposes for debugging. */
async function browserWsUrl() {
  for (let attempt = 0; attempt < 30; attempt += 1) {
    try {
      const res = await fetch('http://127.0.0.1:9222/json/version');
      const json = await res.json();
      if (json.webSocketDebuggerUrl) return json.webSocketDebuggerUrl;
    } catch {
      // Chrome is still starting.
    }
    await sleep(500);
  }
  throw new Error('Chrome is not exposing a debugging endpoint on 127.0.0.1:9222');
}

const browser = await Cdp.connect(await browserWsUrl());
const { targetId } = await browser.send('Target.createTarget', { url: 'about:blank' });
const { sessionId } = await browser.send('Target.attachToTarget', { targetId, flatten: true });
const send = (method, params) => browser.send(method, params, sessionId);

await send('Page.enable');
await send('Runtime.enable');
await send('Emulation.setDeviceMetricsOverride', {
  width: 1600,
  height: 1000,
  deviceScaleFactor: dpr,
  mobile: false,
});
await send('Page.navigate', { url });
// The app opens an SSE stream, so "network idle" never happens; a fixed settle is what works.
await sleep(settle);

/** Run a snippet in the page and report whether it found what it was looking for. */
async function evaluate(expression, { awaitPromise = false } = {}) {
  const result = await send('Runtime.evaluate', { expression, returnByValue: true, awaitPromise });
  if (result.exceptionDetails) {
    throw new Error(`page error: ${result.exceptionDetails.text}`);
  }
  return result.result.value;
}

const shot = async (name) => {
  const { data } = await send('Page.captureScreenshot', { format: 'png' });
  const path = join(outDir, `${name}.png`);
  writeFileSync(path, Buffer.from(data, 'base64'));
  console.log(`  ${name}`);
  return path;
};

/** Click a control by its accessible title, which is what a person reads. */
async function clickByTitle(prefix) {
  const ok = await evaluate(`
    (() => {
      const el = [...document.querySelectorAll('[title]')].find((n) => n.getAttribute('title').startsWith(${JSON.stringify(prefix)}));
      if (!el) return false;
      el.click();
      return true;
    })()`);
  if (!ok) throw new Error(`no control with title starting ${JSON.stringify(prefix)}`);
  await sleep(600);
}

/** Click a flow row in the list; the rows carry their virtualised offset as an inline style. */
async function clickFirstFlow() {
  const ok = await evaluate(`
    (() => {
      const row = document.querySelector('div[style*="top:"]');
      if (!row) return false;
      row.click();
      return true;
    })()`);
  if (!ok) throw new Error('the flow list is empty, so there is no detail view to show');
  await sleep(900);
}

/**
 * Print the computed style of the elements a person is looking at.
 *
 * A screenshot says "something here is dark" and leaves you guessing which element and which rule;
 * this says which one. Both are needed: the picture finds the problem, the measurement explains it.
 */
async function probe(label, selectors) {
  const report = await evaluate(`
    (() => {
      const out = {};
      for (const [name, sel] of Object.entries(${JSON.stringify(selectors)})) {
        const el = document.querySelector(sel);
        if (!el) { out[name] = 'NOT FOUND'; continue; }
        const cs = getComputedStyle(el);
        out[name] = {
          bg: cs.backgroundColor,
          color: cs.color,
          fontSize: cs.fontSize,
          borderBottom: cs.borderBottomColor,
          class: el.className.toString().slice(0, 120),
        };
      }
      return out;
    })()`);
  console.log(`\n[probe] ${label}`);
  for (const [name, value] of Object.entries(report)) {
    console.log(`  ${name}: ${JSON.stringify(value)}`);
  }
  return report;
}

/**
 * Find every element painting a dark background.
 *
 * Selector guessing failed once already — a "right pane" selector simply did not match, so that probe
 * reported nothing and looked like a clean result. Scanning for the *property* instead of the
 * *structure* finds the culprit whatever the markup happens to be, and names it by depth, tag and
 * class so it can be located in source.
 */
async function scanDarkElements(label) {
  // Written as one in-page function with simple, explicit branches: an earlier version filtered with a
  // null sentinel and ended up printing the elements it meant to skip, which is how a diagnostic
  // becomes noise that hides the thing it was written to find.
  const found = await evaluate(`
    (() => {
      function parseColor(value) {
        const m = String(value).match(/rgba?\\(([^)]+)\\)/);
        if (!m) return null;
        const parts = m[1].split(',').map((v) => parseFloat(v.trim()));
        if (parts.length < 3 || parts.some((v) => Number.isNaN(v))) return null;
        const alpha = parts.length > 3 ? parts[3] : 1;
        return { r: parts[0], g: parts[1], b: parts[2], alpha };
      }
      const out = [];
      for (const el of document.querySelectorAll('*')) {
        const cs = getComputedStyle(el);
        const c = parseColor(cs.backgroundColor);
        if (c === null) continue;              // not a plain colour: nothing to judge
        if (c.alpha < 0.5) continue;           // translucent: not what paints a solid dark area
        const lum = (0.2126 * c.r + 0.7152 * c.g + 0.0722 * c.b) / 255;
        if (lum >= 0.35) continue;             // light enough to be fine in a light theme
        const r = el.getBoundingClientRect();
        if (r.width < 40 || r.height < 40) continue;
        out.push({
          tag: el.tagName.toLowerCase(),
          cls: (el.className.toString() || '').slice(0, 90),
          bg: cs.backgroundColor,
          size: Math.round(r.width) + 'x' + Math.round(r.height),
          lum: Number(lum.toFixed(3)),
        });
      }
      return out;
    })()`);
  // Only the light theme is judged: a dark theme is expected to contain dark elements, and printing a
  // count for it invites the number to be read as a defect.
  if (label !== 'light') {
    console.log(`\n[dark scan] ${label}: skipped (a dark theme is supposed to be dark)`);
    return found;
  }
  console.log(`\n[dark scan] ${label}: ${found.length} dark element(s) that should not be dark`);
  for (const el of found) console.log(`  ${el.size}  lum=${el.lum}  ${el.bg}  <${el.tag} class="${el.cls}">`);
  return found;
}

/**
 * Walk the tab order and report, for each stop, whether focus is actually *visible*.
 *
 * "Is there a focus ring" cannot be read off the stylesheet: it depends on what the element computes
 * to when focused versus not, and a `focus:outline-none` anywhere in the chain silently removes it.
 * So this presses Tab for real, then measures the focused element twice — focused and blurred — and
 * reports the difference. An element whose style does not change when it takes focus is a stop a
 * keyboard user cannot see.
 */
async function focusSweep(label = '', limit = 25) {
  const stops = [];
  // Start from a known place.
  await evaluate(`document.activeElement && document.activeElement.blur && document.activeElement.blur()`);

  for (let i = 0; i < limit; i += 1) {
    await send('Input.dispatchKeyEvent', {
      type: 'rawKeyDown',
      key: 'Tab',
      code: 'Tab',
      windowsVirtualKeyCode: 9,
      nativeVirtualKeyCode: 9,
    });
    await send('Input.dispatchKeyEvent', {
      type: 'keyUp',
      key: 'Tab',
      code: 'Tab',
      windowsVirtualKeyCode: 9,
      nativeVirtualKeyCode: 9,
    });
    await sleep(60);

    const info = await evaluate(
      `
      (async () => {
        const el = document.activeElement;
        if (!el || el === document.body) return null;
        const snap = () => {
          const cs = getComputedStyle(el);
          return {
            outline: cs.outlineStyle + ' ' + cs.outlineWidth + ' ' + cs.outlineColor,
            shadow: cs.boxShadow,
            border: cs.borderTopColor + '|' + cs.borderBottomColor,
            background: cs.backgroundColor,
          };
        };
        const focused = snap();
        el.blur();
        await new Promise((r) => requestAnimationFrame(r));
        const blurred = snap();
        el.focus();
        await new Promise((r) => requestAnimationFrame(r));
        return {
          tag: el.tagName.toLowerCase(),
          cls: (el.className.toString() || '').slice(0, 70),
          label: (el.getAttribute('aria-label') || el.getAttribute('title') || el.textContent || '')
            .trim()
            .slice(0, 32),
          focused,
          blurred,
          changed:
            focused.outline !== blurred.outline ||
            focused.shadow !== blurred.shadow ||
            focused.border !== blurred.border ||
            focused.background !== blurred.background,
        };
      })()`,
      { awaitPromise: true }
    );
    if (!info) break;
    // A sweep that cannot describe what it focused has measured nothing; say so instead of reporting
    // every stop as unmarked, which reads exactly like a finding.
    if (!info.tag) {
      console.error('\n[focus sweep] FAILED: could not read the focused element (tool error, not a finding)');
      return [];
    }
    stops.push(info);
  }

  const invisible = stops.filter((s) => !s.changed);
  console.log(
    `\n[focus sweep] ${label}: ${stops.length} tab stop(s), ${invisible.length} with no visible focus`
  );
  for (const stop of stops) {
    const mark = stop.changed ? '  ok ' : '  -->';
    const why = stop.changed
      ? [
          stop.focused.outline !== stop.blurred.outline ? `outline ${stop.focused.outline}` : null,
          stop.focused.shadow !== stop.blurred.shadow ? 'box-shadow' : null,
          stop.focused.border !== stop.blurred.border ? 'border' : null,
          stop.focused.background !== stop.blurred.background ? 'background' : null,
        ]
          .filter(Boolean)
          .join(', ')
      : '(style identical when focused)';
    console.log(`${mark} <${stop.tag}> "${stop.label}" — ${why}`);
    if (!stop.changed) console.log(`        class="${stop.cls}"`);
  }
  return stops;
}

/** Selectors worth measuring together: the list, an even row, an odd row, and the panels. */
const PROBE_TARGETS = {
  body: 'body',
  rowEven: 'div[style*="top: 0px"]',
  rowOdd: 'div[style*="top: 32px"]',
  listHeader: 'input',
  rightPane: 'div[class*="flex-1"][class*="border-l"]',
};

console.log(`capturing into ${outDir}`);

// The views a person moves between, each reached the way they would reach it.
await shot('01-flows-dark');
await clickFirstFlow();
await shot('02-flow-detail-dark');
await clickByTitle('Workshop');
await shot('03-workshop-dark');
await clickByTitle('Rules');
await shot('04-rules-dark');
await clickByTitle('Scripts');
await shot('05-scripts-dark');
await clickByTitle('Settings');
await shot('06-settings-dark');

// The light theme, reached by the toggle rather than by rewriting the page.
await clickByTitle('Switch to');
await clickByTitle('Flows');
await shot('07-flows-light');
await clickFirstFlow();
await shot('08-flow-detail-light');

// The command palette is reachable from anywhere and is easy to forget when reviewing by eye.
await evaluate(`document.dispatchEvent(new KeyboardEvent('keydown', { key: 'k', metaKey: true, bubbles: true }))`);
await sleep(500);
await shot('09-command-palette');
await evaluate(`document.dispatchEvent(new KeyboardEvent('keydown', { key: 'Escape', bubbles: true }))`);
await sleep(400);

// Back to the dark theme and the help overlay, so a run leaves the two ends of the UI captured.
await clickByTitle('Switch to');
await clickByTitle('Help');
await shot('10-help-dark');

// Measure both themes deterministically: set the stored choice and reload, rather than toggling and
// trusting that the label matches what the last click left behind (it did not, first time round).
for (const theme of ['dark', 'light']) {
  await evaluate(`localStorage.setItem('relay-core.theme', ${JSON.stringify(theme)})`);
  await send('Page.reload');
  await sleep(settle);
  await probe(theme, PROBE_TARGETS);
  await scanDarkElements(theme);
  await clickByTitle('Flows');
  await focusSweep(`${theme}/flows`, 12);
  await clickByTitle('Settings');
  await focusSweep(`${theme}/settings`, 14);
  await clickByTitle('Rules');
  await focusSweep(`${theme}/rules`, 14);
  await clickByTitle('Flows');
  await clickFirstFlow();
  await focusSweep(`${theme}/detail`, 14);
}

await browser.send('Target.closeTarget', { targetId });
console.log('done');
process.exit(0);
