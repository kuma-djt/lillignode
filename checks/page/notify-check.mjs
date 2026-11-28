// In a real browser with no window: the page's notification, off unless
// "page_notify": true. The real Lillignode with two stand-in agents. The
// browser is given leave to notify by the check itself, and the page's
// Notification is watched, not shown: what is checked is what the page
// asks the browser to show.
//
//   1. without the setting: no offer, and nothing is shown;
//   2. with it: the offer, once; Not now keeps it away;
//   3. turned on: one notification, naming the agent, when it comes to
//      need the reader, and no second one while it still waits;
//   4. clicking it opens that agent; a new wait is a new notification.
//
//   node checks/page/notify-check.mjs [folder for pictures]
//
// Needs: Node 22 or later, Edge or Chrome.
import { spawn } from "node:child_process";
import { mkdtempSync, mkdirSync, rmSync, writeFileSync, existsSync } from "node:fs";
import { tmpdir } from "node:os";
import { join, resolve, dirname } from "node:path";
import { fileURLToPath } from "node:url";
import { createServer } from "node:net";

const here = dirname(fileURLToPath(import.meta.url));
const engine = resolve(process.env.LILLIGNODE_ENGINE ?? resolve(here, "../.."));
const pictures = resolve(process.argv[2] ?? join(here, "notify-out"));
mkdirSync(pictures, { recursive: true });

const browserPath = [
  "C:/Program Files (x86)/Microsoft/Edge/Application/msedge.exe",
  "C:/Program Files/Microsoft/Edge/Application/msedge.exe",
  "C:/Program Files/Google/Chrome/Application/chrome.exe",
  "/usr/bin/google-chrome",
  "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome",
].find(existsSync);
if (!browserPath) throw new Error("No Edge or Chrome found.");

let failed = 0;
function check(step, ok, detail = "") {
  console.log(`  ${ok ? "ok  " : "FAIL"}  ${step}${ok || !detail ? "" : "\n        " + detail}`);
  if (!ok) failed += 1;
}
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
const freePort = () =>
  new Promise((done) => {
    const s = createServer();
    s.listen(0, "127.0.0.1", () => {
      const { port } = s.address();
      s.close(() => done(port));
    });
  });

// --- the real Lillignode, serving the page ------------------------------
const ext = process.platform === "win32" ? ".exe" : "";
let out = "";
const waiters = [];
let serving = null;
function startServing(settings) {
  out = "";
  serving = spawn(join(engine, "target/release/examples/page_check" + ext), ["--serve", "--type"], {
    stdio: ["pipe", "pipe", "inherit"],
    env: settings ? { ...process.env, PAGE_CHECK_SETTINGS: settings } : { ...process.env },
  });
  serving.stdout.on("data", (d) => {
    out += d;
    for (const w of [...waiters]) w();
  });
}
async function stopServing() {
  if (!serving) return;
  try {
    serving.stdin.write("quit\n");
    await Promise.race([line(/DONE quit/, 20000), sleep(20000)]);
  } catch {}
  serving.kill();
  serving = null;
  await sleep(500);
}
const tell = async (command, done) => {
  serving.stdin.write(command + "\n");
  await line(done, 30000);
};
function line(pattern, limit = 60000) {
  return new Promise((done, fail) => {
    const timer = setTimeout(() => fail(new Error("no " + pattern + " from the page check:\n" + out)), limit);
    const look = () => {
      const m = out.match(pattern);
      if (m) {
        clearTimeout(timer);
        waiters.splice(waiters.indexOf(look), 1);
        done(m);
      }
    };
    waiters.push(look);
    look();
  });
}

// --- the browser -------------------------------------------------------
const profile = mkdtempSync(join(tmpdir(), "lillignode-design-browser-"));
const debugPort = await freePort();
const browser = spawn(
  browserPath,
  ["--headless=new", `--remote-debugging-port=${debugPort}`, `--user-data-dir=${profile}`, "--no-first-run",
    "--no-default-browser-check", "--disable-extensions", "--disable-gpu", "--window-size=1400,900",
    // GitHub's Linux machines cannot give Chrome its sandbox; only there.
    ...(process.env.CI && process.platform === "linux" ? ["--no-sandbox"] : []),
    "about:blank"],
  { stdio: "ignore" },
);
let ws;
let nextId = 0;
const pending = new Map();
function send(method, params = {}) {
  const id = ++nextId;
  ws.send(JSON.stringify({ id, method, params }));
  return new Promise((done, fail) => pending.set(id, { done, fail }));
}
async function evaluate(expression) {
  const r = await send("Runtime.evaluate", { expression, returnByValue: true, awaitPromise: true });
  if (r.exceptionDetails) throw new Error(JSON.stringify(r.exceptionDetails));
  return r.result.value;
}
async function picture(name) {
  const shot = await send("Page.captureScreenshot", { format: "png", captureBeyondViewport: false });
  writeFileSync(join(pictures, name + ".png"), Buffer.from(shot.data, "base64"));
}
async function width(w, h) {
  await send("Emulation.setDeviceMetricsOverride", { width: w, height: h, deviceScaleFactor: 1, mobile: w < 600 });
  await sleep(600);
}
async function until(what, test, limit = 20000) {
  const end = Date.now() + limit;
  let seen;
  while (Date.now() < end) {
    seen = await evaluate(what);
    if (test(seen)) return seen;
    await sleep(250);
  }
  return seen;
}
const text = (sel) => `(() => { const e = document.querySelector(${JSON.stringify(sel)}); return e && !e.closest("[hidden]") && e.innerText.trim() ? e.innerText : null; })()`;
const cardText = (id) => `(() => { const li = document.querySelector('#board li[data-id="${id}"]'); return li ? li.innerText : null; })()`;
const visible = (sel) => `(() => { const e = document.querySelector(${JSON.stringify(sel)}); return !!e && !e.closest("[hidden]") && e.getClientRects().length > 0 && getComputedStyle(e).visibility !== "hidden"; })()`;
// Press a button inside `scope` whose words match the pattern given as text.
const press = (scope, pattern) => `(() => { const b = [...document.querySelectorAll(${JSON.stringify(scope + " button")})].find((x) => ${pattern}.test(x.textContent.trim()) && !x.disabled && x.getClientRects().length); if (b) b.click(); return !!b; })()`;
// How many places on the page say this, outside the terminal.
const saying = (words) => `[...document.querySelectorAll("body *")].filter((e) => !e.closest("#terminal") && e.getClientRects().length && [...e.childNodes].some((c) => c.nodeType === 3 && c.textContent.includes(${JSON.stringify(words)}))).length`;


async function cleanup() {
  await stopServing();
  browser.kill();
  await sleep(500);
  try {
    rmSync(profile, { recursive: true, force: true });
  } catch {}
}

// Every Notification the page makes is kept, not shown.
const WATCH = `(() => {
  const Real = window.Notification;
  const shown = [];
  window.__shown = shown;
  function Watched(title, options) {
    const n = { title, body: options && options.body, silent: options && options.silent, onclick: null, close() { n.closed = true; } };
    shown.push(n);
    return n;
  }
  Watched.requestPermission = (...a) => Real.requestPermission(...a);
  Object.defineProperty(Watched, "permission", { get: () => Real.permission });
  window.Notification = Watched;
})();`;
const shownCount = `window.__shown.length`;

try {
  let target;
  for (let i = 0; i < 300 && !target; i += 1) {
    try {
      target = (await (await fetch(`http://127.0.0.1:${debugPort}/json/list`)).json()).find((t) => t.type === "page");
    } catch {}
    if (!target) await sleep(100);
  }
  ws = new WebSocket(target.webSocketDebuggerUrl);
  await new Promise((done, fail) => ((ws.onopen = done), (ws.onerror = fail)));
  ws.onmessage = (e) => {
    const m = JSON.parse(e.data);
    if (m.id && pending.has(m.id)) {
      const p = pending.get(m.id);
      pending.delete(m.id);
      m.error ? p.fail(new Error(JSON.stringify(m.error))) : p.done(m.result);
    }
  };
  await send("Page.enable");
  await send("Runtime.enable");
  await send("Page.addScriptToEvaluateOnNewDocument", { source: WATCH });
  await width(1400, 900);

  console.log("1. Without the setting");
  startServing(null);
  let ready = await line(/READY (http:\/\/127\.0\.0\.1:\d+\/[a-z0-9]{16}\/)/);
  await send("Page.navigate", { url: ready[1] });
  await until(`document.getElementById("agents").innerText`, (t) => /reporter/.test(t || ""), 20000);
  await tell("key p", /DONE key/);
  await until(`document.getElementById("agents").innerText`, (t) => /Needs you/i.test(t || ""), 20000);
  await sleep(1500);
  check("no offer to notify", !(await evaluate(visible("#notify-offer"))));
  check("and nothing is shown", (await evaluate(shownCount)) === 0);
  await stopServing();

  console.log("2. With it: the offer, once");
  startServing('"page_notify": true');
  ready = await line(/READY (http:\/\/127\.0\.0\.1:\d+\/[a-z0-9]{16}\/)/);
  await send("Page.navigate", { url: ready[1] });
  await until(`document.getElementById("agents").innerText`, (t) => /reporter/.test(t || ""), 20000);
  await sleep(800);
  check("the page offers to notify", await evaluate(visible("#notify-offer")));
  check("in words that say what for", /when an agent comes to need you/.test((await evaluate(text("#notify-offer"))) || ""));
  await picture("01-the-offer");
  await evaluate(`document.getElementById("notify-later").click()`);
  await send("Page.reload");
  await until(`document.getElementById("agents").innerText`, (t) => /reporter/.test(t || ""), 20000);
  await sleep(800);
  check("Not now: not offered again", !(await evaluate(visible("#notify-offer"))));
  // Offered again only in a browser that was never asked: forget the
  // answer, as a new browser would have none.
  await evaluate(`localStorage.removeItem("lillignode.notify.asked")`);
  await send("Page.reload");
  await until(`document.getElementById("agents").innerText`, (t) => /reporter/.test(t || ""), 20000);
  await sleep(800);

  console.log("3. Turned on: one notification for one wait");
  await send("Browser.grantPermissions", { permissions: ["notifications"], origin: new URL(ready[1]).origin });
  await evaluate(`document.getElementById("notify-on").click()`);
  await sleep(800);
  check("once turned on, the offer is gone", !(await evaluate(visible("#notify-offer"))));
  check("nothing yet: no agent needs you", (await evaluate(shownCount)) === 0);
  await tell("key p", /DONE key/);
  const one = await until(shownCount, (n) => n >= 1, 15000);
  check("when an agent comes to need you, one notification", one === 1, String(one));
  const first = await evaluate(`window.__shown[0]`);
  check("naming it, with a sound", /^reporter \(work\) needs you$/.test(first.title) && first.silent === false, JSON.stringify(first));
  await sleep(4000);
  check("and no second one while it still waits", (await evaluate(shownCount)) === 1, String(await evaluate(shownCount)));

  console.log("4. Clicking it opens that agent; a new wait is a new one");
  await evaluate(`[...document.querySelectorAll("#agents li")].find((li) => /plain/.test(li.innerText)).firstChild.click()`);
  await sleep(500);
  await evaluate(`window.__shown[0].onclick()`);
  await sleep(800);
  check("clicking it opens that agent", /reporter/.test((await evaluate(text("#terminal-title"))) || ""));
  check("and closes it", (await evaluate(`window.__shown[0].closed === true`)) === true);
  await tell("key w", /DONE key/);
  await sleep(1500);
  await tell("key p", /DONE key/);
  const two = await until(shownCount, (n) => n >= 2, 15000);
  check("a new wait: a new notification", two === 2, String(two));
  await picture("02-after");
  await send("Page.reload");
  await sleep(2500);
  check("a page opened while it waits does not notify for that wait", (await evaluate(shownCount)) === 0);
} catch (e) {
  console.log("  FAIL  " + e.message);
  failed += 1;
} finally {
  await cleanup();
}
console.log(failed ? `\n${failed} checks failed.` : "\nAll notification checks passed.");
process.exit(failed ? 1 : 0);
