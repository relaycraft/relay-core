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
 * Requires Chrome running with --remote-debugging-port=$CDP_PORT (default 9222);
 * scripts/ui-fixtures.sh starts it. Set CDP_PORT when something else already owns 9222 — a developer's
 * own browser does — because attaching to *that* browser photographs their tabs and reports it as a
 * review of this UI.
 */
import { writeFileSync, mkdirSync } from 'node:fs';
import { join } from 'node:path';

const [url, outDir] = process.argv.slice(2);
const cdpPort = Number(process.env.CDP_PORT ?? 9222);
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
    this.listeners = new Map();
    ws.addEventListener('message', (event) => {
      const msg = JSON.parse(event.data);
      if (msg.id && this.pending.has(msg.id)) {
        const { resolve, reject } = this.pending.get(msg.id);
        this.pending.delete(msg.id);
        msg.error ? reject(new Error(`${msg.error.message} (${JSON.stringify(msg.error.data)})`)) : resolve(msg.result);
        return;
      }
      for (const handler of this.listeners.get(msg.method) ?? []) handler(msg.params, msg.sessionId);
    });
  }

  /** Subscribe to a CDP event (no `id`), e.g. `Runtime.exceptionThrown`. */
  on(method, handler) {
    if (!this.listeners.has(method)) this.listeners.set(method, []);
    this.listeners.get(method).push(handler);
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
      const res = await fetch(`http://127.0.0.1:${cdpPort}/json/version`);
      const json = await res.json();
      if (json.webSocketDebuggerUrl) return json.webSocketDebuggerUrl;
    } catch {
      // Chrome is still starting.
    }
    await sleep(500);
  }
  throw new Error(`Chrome is not exposing a debugging endpoint on 127.0.0.1:${cdpPort}`);
}

const browser = await Cdp.connect(await browserWsUrl());

/**
 * Refuse to drive a browser that is not the harness's own.
 *
 * The debugging port is a fixed number, so anything already on it wins: a developer's everyday Chrome
 * does, and then every screenshot in the output directory is of *their* tabs while the report reads
 * like a review of the UI. A fresh harness browser has no page open at all, which is what makes the
 * check possible. Failing here costs one run; not failing produced findings about the wrong screen.
 */
const { targetInfos } = await browser.send('Target.getTargets');
const strays = targetInfos.filter((t) => t.type === 'page' && t.url !== 'about:blank');
if (strays.length > 0) {
  throw new Error(
    `the browser on port ${cdpPort} already has ${strays.length} page(s) open (e.g. ${strays[0].url}); ` +
      `it is not this harness's browser — set CDP_PORT to a free port`
  );
}

const { targetId } = await browser.send('Target.createTarget', { url: 'about:blank' });
const { sessionId } = await browser.send('Target.attachToTarget', { targetId, flatten: true });
const send = (method, params) => browser.send(method, params, sessionId);

/**
 * Every uncaught page exception, in order.
 *
 * A thrown error inside a Solid update does not stop at the component that threw: it aborts the
 * update queue, so *later, unrelated* interactions silently do nothing. The user calls that "the UI
 * froze", and a screenshot of the frozen state looks like a normal screen. Collecting the exceptions
 * is what turns that into a finding instead of a mystery — and this harness previously collected
 * none, so a page throwing on every render still produced a clean folder of screenshots.
 */
const pageErrors = [];
browser.on('Runtime.exceptionThrown', (params) => {
  const details = params?.exceptionDetails ?? {};
  const description = details.exception?.description ?? details.text ?? 'unknown error';
  pageErrors.push(`${description.split('\n')[0]} — ${details.url ?? '?'}:${details.lineNumber ?? '?'}`);
});

/** Defects found by the checks below. A non-empty list makes the run exit non-zero. */
const faults = [];
const fault = (message) => {
  faults.push(message);
  console.log(`  FAIL ${message}`);
};

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

/** Click the flow row whose text contains this marker, so a shot can target a known body. */
async function clickFlowMatching(marker) {
  const ok = await evaluate(`
    (() => {
      const rows = [...document.querySelectorAll('div[style*="top:"]')];
      const row = rows.find((r) => r.textContent.includes(${JSON.stringify(marker)}));
      if (!row) return false;
      row.click();
      return true;
    })()`);
  if (!ok) throw new Error(`no flow row containing ${JSON.stringify(marker)}`);
  await sleep(800);
}

/** Open the Payload tab of whatever flow is selected. */
async function openPayloadTab() {
  const ok = await evaluate(`
    (() => {
      const tab = [...document.querySelectorAll('button')].find((b) => b.textContent.trim() === 'Payload');
      if (!tab) return false;
      tab.click();
      return true;
    })()`);
  if (!ok) throw new Error('no Payload tab');
  await sleep(500);
}

/** Put the page in a known theme the honest way: store the choice the app reads, then reload. */
async function setTheme(theme) {
  await evaluate(`localStorage.setItem('relay-core.theme', ${JSON.stringify(theme)})`);
  await send('Page.reload');
  await sleep(settle);
}

/**
 * Read the detail pane: which tab the strip says is open, and what the pane below it actually shows.
 *
 * Both halves matter. A click that writes its signal but never re-renders leaves the strip showing
 * the new tab while the content is still the old one — which is exactly what a wedged Solid update
 * queue looks like, and it is invisible in a screenshot because the highlighted tab is correct.
 */
async function readDetailPane() {
  return evaluate(`
    (() => {
      const labels = ['headers', 'payload', 'timing', 'messages', 'trace'];
      const buttons = [...document.querySelectorAll('button')].filter((b) =>
        labels.includes(b.textContent.trim().toLowerCase())
      );
      if (buttons.length === 0) return null;
      const strip = buttons[0].parentElement;
      const pane = strip && strip.nextElementSibling;
      const active = buttons.find((b) => b.getAttribute('aria-current') === 'page');
      return {
        tabs: buttons.map((b) => b.textContent.trim()),
        active: active ? active.textContent.trim() : null,
        // The measured window is the pane itself: the tab strip's next sibling is the scroll
        // container the Switch renders into, so this cannot pick up a neighbouring panel by accident.
        pane: pane ? pane.textContent.replace(/\\s+/g, ' ').trim().slice(0, 200) : null,
      };
    })()`);
}

/**
 * Click every tab in the detail pane and check the pane actually followed.
 *
 * This is the regression check for the "Trace freezes the UI" defect: clicking Trace used to throw
 * while reading a field the API omits when it is empty, and because that throw happened inside a
 * Solid update it killed the whole update queue — the tab highlighted, the content did not change,
 * and every later click in the pane was a no-op.
 */
async function checkDetailTabs() {
  const first = await readDetailPane();
  // A check that cannot see the thing it measures must say so: an empty result here would otherwise
  // read as "no problems found".
  if (!first) throw new Error('no detail tab strip on screen — is a flow selected?');
  console.log(`\n[detail tabs] clicking ${first.tabs.length} tab(s): ${first.tabs.join(', ')}`);

  const panes = new Map();
  for (const label of first.tabs) {
    const clicked = await evaluate(`
      (() => {
        const b = [...document.querySelectorAll('button')].find((n) => n.textContent.trim() === ${JSON.stringify(label)});
        if (!b) return false;
        b.click();
        return true;
      })()`);
    if (!clicked) {
      fault(`${label}: the tab vanished between reading the strip and clicking it`);
      continue;
    }
    await sleep(350);
    const now = await readDetailPane();
    console.log(`  ${label}: strip says "${now?.active}" — pane "${(now?.pane ?? '').slice(0, 60)}"`);
    if (!now) {
      fault(`${label}: the tab strip disappeared after the click`);
      continue;
    }
    if (now.active !== label) {
      fault(`${label}: clicked, but the strip still shows "${now.active}" as open`);
    }
    if (!now.pane) {
      fault(`${label}: rendered an empty pane`);
      continue;
    }
    if (now.pane.includes('This view could not be rendered')) {
      fault(`${label}: the view threw and was contained — pane says "${now.pane.slice(0, 120)}"`);
      continue;
    }
    const previous = panes.get(now.pane);
    if (previous) {
      fault(`${label}: the pane still shows ${previous}'s content — the click changed nothing`);
    } else {
      panes.set(now.pane, label);
    }
  }
}

/**
 * Check that the drawn list is in the order the API returned it.
 *
 * The API page is newest-first, so anything that applies that page one row at a time with an
 * unshift ends up reversed — and the symptom only shows after a refresh, which is why it survived
 * until someone reported it. Rows are read from the DOM rather than from the store, because what a
 * person sees is the point.
 */
async function checkFlowOrder() {
  const drawn = await evaluate(`
    (() => {
      const rows = [...document.querySelectorAll('div[style*="top:"]')]
        .sort((a, b) => parseFloat(a.style.top) - parseFloat(b.style.top));
      return rows.map((row) => {
        const spans = row.querySelectorAll('span');
        // method | status | host+path: fixed columns, but if the markup changes the shape must be
        // reported as a tool error rather than compared as if it were a list of flows.
        if (spans.length < 4) return null;
        return spans[1].textContent.trim() + ' ' + spans[3].textContent.trim().replace(/\\s+/g, '');
      });
    })()`);
  if (drawn.length === 0 || drawn.some((r) => r === null)) {
    throw new Error(`could not read the flow rows (${drawn.length} rows, ${drawn.filter((r) => r === null).length} unreadable)`);
  }
  const fromApi = await evaluate(
    `fetch('/api/v1/flows?limit=100').then((r) => (r.ok ? r.json() : Promise.reject(new Error('HTTP ' + r.status))))
       .then((j) => (j.items || []).map((f) => f.method + ' ' + (f.host + f.path).replace(/\\s+/g, '')))`,
    { awaitPromise: true }
  );
  const n = Math.min(drawn.length, fromApi.length);
  console.log(`\n[flow order] ${n} row(s) compared against the API page (newest first)`);
  let mismatches = 0;
  for (let i = 0; i < n; i += 1) {
    if (drawn[i] !== fromApi[i]) {
      mismatches += 1;
      if (mismatches <= 3) fault(`flow order: row ${i} is "${drawn[i]}" but the API has "${fromApi[i]}"`);
    }
  }
  if (mismatches > 3) fault(`flow order: ${mismatches} of ${n} rows are in the wrong place`);
  if (mismatches === 0) console.log(`  ok — the drawn order matches the API order`);
}

/**
 * Check the interface still reacts at all.
 *
 * An exception thrown inside a Solid update wedges the update queue, and the way a person meets that
 * is "nothing responds any more". Reading the body text before and after a view change is the
 * cheapest honest test of that: if the queue is dead, the text does not move.
 */
async function checkStillAlive() {
  const before = await evaluate(`document.body.innerText.replace(/\\s+/g, ' ').slice(0, 400)`);
  await clickByTitle('Settings');
  const after = await evaluate(`document.body.innerText.replace(/\\s+/g, ' ').slice(0, 400)`);
  if (before === after) {
    fault('the view did not change after clicking Settings — reactive updates are wedged');
  } else {
    console.log('\n[still alive] ok — switching views still updates the screen');
  }
  await clickByTitle('Flows');
}

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
  await setTheme(theme);
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

// Behaviour, not appearance: the two checks below are the regressions that screenshots alone did not
// catch. Run them on a freshly loaded page — the flow-order defect only appears after a reload.
// The theme is set explicitly because the loop above ends on light, and every later filename says
// `-dark`: without this the shots were labelled dark while showing the light theme.
await setTheme('dark');
await clickByTitle('Flows');
await checkFlowOrder();
await clickFirstFlow();
await checkDetailTabs();
await checkStillAlive();

// A narrow window: every other shot is 1600x1000, so nothing has ever checked that the layout holds
// when it cannot. Fixed widths in the list, the palette and the toolbar all assume room.
await send('Emulation.setDeviceMetricsOverride', {
  width: 1024,
  height: 700,
  deviceScaleFactor: dpr,
  mobile: false,
});
await clickByTitle('Flows');
await sleep(500);
await shot('11-narrow-flows-dark');
await clickFirstFlow();
await shot('12-narrow-detail-dark');
await evaluate(
  `document.dispatchEvent(new KeyboardEvent('keydown', { key: 'k', metaKey: true, bubbles: true }))`
);
await sleep(400);
await shot('13-narrow-palette-dark');
// Close the palette and restore the window: leaving either behind silently changed every later shot,
// which is how the body shots came out narrow with a palette covering the pane they were meant to show.
await evaluate(`document.dispatchEvent(new KeyboardEvent('keydown', { key: 'Escape', bubbles: true }))`);
await sleep(400);
await send('Emulation.setDeviceMetricsOverride', {
  width: 1600,
  height: 1000,
  deviceScaleFactor: dpr,
  mobile: false,
});
await sleep(400);

// Bodies that are not text: an image must be drawn, and binary must not be dumped as mojibake.
await clickByTitle('Flows');
await clickFlowMatching('/png');
await openPayloadTab();
await shot('14-image-body-dark');
await clickFlowMatching('/binary');
await openPayloadTab();
await shot('15-binary-body-dark');
await clickFlowMatching('/hello');
await openPayloadTab();
await shot('16-json-body-dark');

/**
 * The verdict, printed last so it is the thing left on screen.
 *
 * An uncaught exception is always a finding: it is either breaking this run's rendering or wedging
 * the reactive update queue for whatever the user does next, and the frozen result photographs
 * perfectly. Exiting non-zero is deliberate — `ui-fixtures.sh` stops on it, so a broken UI cannot
 * leave behind a folder of plausible screenshots and a "done".
 */
console.log(`\n[page errors] ${pageErrors.length} uncaught exception(s)`);
for (const error of pageErrors) console.log(`  ${error}`);

await browser.send('Target.closeTarget', { targetId });
if (faults.length > 0 || pageErrors.length > 0) {
  console.error(`\n[failed] ${faults.length} check(s) and ${pageErrors.length} uncaught error(s); see above`);
  process.exit(1);
}
console.log('done');
process.exit(0);
