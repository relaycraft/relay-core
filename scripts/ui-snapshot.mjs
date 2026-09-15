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
async function evaluate(expression) {
  const result = await send('Runtime.evaluate', { expression, returnByValue: true });
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

// Back to the dark theme and the help overlay, so a run leaves the two ends of the UI captured.
await clickByTitle('Switch to');
await clickByTitle('Help');
await shot('09-help-dark');

// Measure the same elements in both themes: a picture shows that something is off, these numbers say
// which element and which declared colour is responsible.
await clickByTitle('Switch to');
await clickByTitle('Flows');
await sleep(400);
await probe('dark', PROBE_TARGETS);
await clickByTitle('Switch to');
await sleep(600);
await probe('light', PROBE_TARGETS);

await browser.send('Target.closeTarget', { targetId });
console.log('done');
process.exit(0);
