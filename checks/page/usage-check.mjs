// In a real browser with no window: a usage limit on the page.
// The real Lillignode with a stand-in agent that reports one, with
// Lillignode's own keys (limit=usage, resets=). No real agent.
//
//   node checks/page/usage-check.mjs [folder for pictures]
//
// Needs: Node 22 or later, Edge or Chrome.
import { spawn } from "node:child_process";
import { mkdtempSync, mkdirSync, rmSync, writeFileSync, existsSync } from "node:fs";
import { tmpdir } from "node:os";
import { join, resolve, dirname } from "node:path";
import { fileURLToPath } from "node:url";
import { createServer } from "node:net";

const here = dirname(fileURLToPath(import.meta.url));
const engine = resolve(here, "../..");
const pictures = resolve(process.argv[2] ?? join(here, "usage-out"));
mkdirSync(join(pictures, "frames"), { recursive: true });

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

// --- the real Lillignode, serving the page, with the adapter named ------
const ext = process.platform === "win32" ? ".exe" : "";
let out = "";
const waiters = [];
const serving = spawn(join(engine, "target/release/examples/page_check" + ext), ["--serve"], {
  stdio: ["pipe", "pipe", "inherit"],
});
serving.stdout.on("data", (d) => {
  out += d;
  for (const w of [...waiters]) w();
});
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
const profile = mkdtempSync(join(tmpdir(), "lillignode-adapter-browser-"));
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
let frame = 0;
let filming = false;
async function film(ms) {
  filming = true;
  const end = Date.now() + ms;
  while (Date.now() < end && filming) {
    const shot = await send("Page.captureScreenshot", { format: "png" });
    frame += 1;
    writeFileSync(join(pictures, "frames", String(frame).padStart(4, "0") + ".png"), Buffer.from(shot.data, "base64"));
    await sleep(400);
  }
}
const text = (sel) => `(() => { const e = document.querySelector(${JSON.stringify(sel)}); return e && !e.closest("[hidden]") ? e.innerText : null; })()`;
const cardText = (id) => `(() => { const li = document.querySelector('#board li[data-id="${id}"]'); return li ? li.innerText : null; })()`;

async function cleanup() {
  try {
    serving.stdin.write("quit\n");
    await Promise.race([line(/DONE quit/, 20000), sleep(20000)]);
  } catch {}
  serving.kill();
  browser.kill();
  await sleep(500);
  try {
    rmSync(profile, { recursive: true, force: true });
  } catch {}
}

try {
  const ready = await line(/READY (http:\/\/127\.0\.0\.1:\d+\/[a-z0-9]{16}\/)/);
  const address = ready[1];
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
  await width(1400, 900);
  await send("Page.navigate", { url: address });

  const visible = (sel) => `(() => { const e = document.querySelector(${JSON.stringify(sel)}); if (!e || e.closest("[hidden]")) return false; const r = e.getBoundingClientRect(); const st = getComputedStyle(e); return r.width > 0 && r.height > 0 && st.visibility !== "hidden" && r.right > 0 && r.left < window.innerWidth; })()`;
  const top = (sel) => `document.querySelector(${JSON.stringify(sel)}).getBoundingClientRect().top`;
  const tap = (sel) => evaluate(`document.querySelector(${JSON.stringify(sel)}).click()`);

  await width(1400, 900);
  await until(text("#agents"), (t) => t !== null && t.includes("reporter"), 30000);

  const row = `(() => { const li = [...document.querySelectorAll("#agents li")].find((l) => l.innerText.includes("reporter")); return li ? li.innerText : null; })()`;
  const rowClass = `(() => { const li = [...document.querySelectorAll("#agents li")].find((l) => l.innerText.includes("reporter")); return li ? li.className : null; })()`;

  console.log("A. A usage limit, with the time it resets");
  await tell("key L", /DONE key L/);
  const limited = await until(row, (t) => (t || "").includes("Usage limit"), 10000);
  check("the agent's row says Usage limit, and when it resets by the reader's clock", /Usage limit, resets \d{1,2}:\d{2}\s?(AM|PM|am|pm)? \(in 2 h [45] min\)/.test(limited || ""), limited);
  check("it is its own state, not marked as needing you", !((await evaluate(rowClass)) || "").includes("needs-you"), await evaluate(rowClass));
  check("and it is not in Blocking now", (await evaluate(text("#blocking"))) === null, await evaluate(text("#blocking")));
  await picture("01-limit");
  const filmed = film(24000);
  await sleep(2500);

  console.log("B. No time given, a time passed, and the limit ending");
  await tell("key N", /DONE key N/);
  const none = await until(row, (t) => (t || "").includes("reset time not given"), 10000);
  check("with no time given, it says so", (none || "").includes("Usage limit, reset time not given"), none);
  await picture("02-no-time");
  await sleep(2500);
  await tell("key X", /DONE key X/);
  const past = await until(row, (t) => (t || "").includes("ago"), 10000);
  check("past the time it gave, it still says what the agent said, and that the time passed", /Usage limit, the time it gave, .+, passed 5 min ago/.test(past || ""), past);
  await picture("03-time-passed");
  await sleep(2500);
  await tell("key i", /DONE key i/);
  const idle = await until(row, (t) => (t || "").includes("Idle"), 10000);
  check("another report ends it", (idle || "").includes("Idle") && !(idle || "").includes("Usage limit"), idle);
  await sleep(2500);
  await tell("key L", /DONE key L/);
  await until(row, (t) => (t || "").includes("Usage limit"), 10000);
  await sleep(3000);
  await filmed;

  console.log("C. A day or more ahead");
  await tell("key D", /DONE key D/);
  const day = await until(row, (t) => (t || "").includes("1 d"), 10000);
  check("a reset a day or more ahead says the day, and counts in days", /Usage limit, resets [A-Z][a-z]{2}, .+ \(in 1 d 3 h\)/.test(day || ""), day);
  await picture("04-a-day-ahead");
  await tell("key L", /DONE key L/);
  await until(row, (t) => (t || "").includes("in 2 h"), 10000);

  console.log("D. At phone width");
  await width(390, 844);
  const phoneRow = await until(row, (t) => (t || "").includes("Usage limit"), 5000);
  check("the phone's list says it too", (phoneRow || "").includes("Usage limit, resets"), phoneRow);
  await picture("05-phone-list");
  await evaluate(`[...document.querySelectorAll("#agents li button")].find((b) => b.innerText.includes("reporter")).click()`);
  const note = await until(text("#terminal-note"), (t) => (t || "").includes("Usage limit"), 5000);
  check("opened on the phone, the agent's own view says it is at a usage limit", (note || "").startsWith("Usage limit, resets"), note);
  await picture("06-phone-agent");
  check("nothing is wider than the phone", !(await evaluate("document.documentElement.scrollWidth > window.innerWidth + 1")));
} catch (e) {
  console.log("  FAIL  " + e.message);
  failed += 1;
} finally {
  await cleanup();
}
console.log(failed ? `\n${failed} checks failed.` : "\nAll usage-limit checks passed.");
process.exit(failed ? 1 : 0);
