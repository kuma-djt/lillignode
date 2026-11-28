// In a real browser with no window: the page in each of its three
// looks ("page_look": soft, cards, bold), with the same stand-in data in
// each: three agents (one working, one that needs you, one that reports
// nothing) and the stand-in Board. Pictures at 1400, 768 and 390 wide. Styling
// only: the checks are that every look keeps what the page does.
//
//   node checks/page/looks-check.mjs [folder for pictures]
//   (LILLIGNODE_ENGINE names another engine checkout to use; the default is this one)
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
const pictures = resolve(process.argv[2] ?? join(here, "looks-out"));
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

// --- the real Lillignode, serving the page, once for each look ----------
const ext = process.platform === "win32" ? ".exe" : "";
let out = "";
const waiters = [];
let serving = null;
function startServing(look) {
  out = "";
  serving = spawn(join(engine, "target/release/examples/page_check" + ext), ["--serve", "--type"], {
    stdio: ["pipe", "pipe", "inherit"],
    env: { ...process.env, PAGE_CHECK_SETTINGS: `"page_look": "${look}"`, PAGE_CHECK_THIRD: "1" },
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
  await stopServing();
  browser.kill();
  await sleep(500);
  try {
    rmSync(profile, { recursive: true, force: true });
  } catch {}
}

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
  const rows = `[...document.querySelectorAll("#agents li")].map((li) => li.className + "|" + li.firstChild.dataset.initials + "|" + li.innerText.replace(/\\s+/g, " ")).join("\\n")`;
  const tall = (sel) => `[...document.querySelectorAll(${JSON.stringify(sel)})].filter((e) => !e.closest("[hidden]") && e.offsetParent).map((e) => Math.round(e.getBoundingClientRect().height))`;
  let n = 0;
  for (const look of ["soft", "cards", "bold"]) {
    console.log(`\n${look}`);
    startServing(look);
    const ready = await line(/READY (http:\/\/127\.0\.0\.1:\d+\/[a-z0-9]{16}\/)/);
    await tell("board standin", /DONE board/);
    // The same states in every look: the newest agent needs you, the one
    // before it is working, the first reports nothing.
    await tell("key p", /DONE key/);
    await tell("front", /DONE front/);
    await tell("front", /DONE front/);
    await tell("key w", /DONE key/);
    await width(1400, 900);
    await send("Page.navigate", { url: ready[1] });
    const shown = await until(rows, (t) => (t || "").includes("needs-you") && (t || "").includes("working"), 20000);
    check(`${look}: three agents, one working and one that needs you`, (shown || "").split("\n").length === 3 && shown.includes("working") && shown.includes("needs-you"), shown);
    const css = await evaluate(`fetch("look.css").then((r) => r.text())`);
    check(`${look}: its stylesheet is served`, css.includes(`Look "${look}"`), css.slice(0, 80));
    const avatar = await evaluate(`getComputedStyle(document.querySelector("#agents li button"), "::before").content`);
    check(`${look}: each row has a round or square slot with the agent's initials`, /"[A-Z?]{1,2}"/.test(avatar || ""), avatar);
    const board = await until(text("#board"), (t) => (t || "").length > 40, 10000);
    check(`${look}: the Board is there`, (board || "").length > 40);
    await sleep(800);
    n += 1;
    await picture(`${String(n).padStart(2, "0")}-${look}-desktop`);
    // Touch-sized buttons: at least 44 pixels tall.
    const buttons = await evaluate(tall("#typing button, #agents li button"));
    check(`${look}: buttons are at least 44 pixels tall`, buttons.length > 0 && Math.min(...buttons) >= 44, JSON.stringify(buttons));
    // A small laptop and a tablet: the terminal keeps a usable width.
    for (const [w, h] of [[1024, 768], [768, 1024]]) {
      await width(w, h);
      await sleep(500);
      const term = await evaluate(`document.getElementById("terminal").getBoundingClientRect().width`);
      check(`${look}: at ${w} wide the terminal is at least 300 pixels wide`, term >= 300, String(term));
      check(`${look}: at ${w} wide nothing scrolls sideways`, !(await evaluate("document.documentElement.scrollWidth > window.innerWidth + 1")));
      if (w === 768) await picture(`${look}-tablet-768`);
    }
    await width(390, 844);
    await sleep(500);
    const standing = await evaluate(`getComputedStyle(document.getElementById("standing"), "::after").content`);
    check(`${look}: at phone width the header says it in a few words`, /^"(Can |Read only)/.test(standing || ""), standing);
    const phoneButtons = await evaluate(tall("#board-open, #agents li button"));
    check(`${look}: on a phone the Board button and the rows are at least 44 pixels tall`, phoneButtons.length > 0 && Math.min(...phoneButtons) >= 44, JSON.stringify(phoneButtons));
    const firstRow = await evaluate(`document.querySelector("#agents li").getBoundingClientRect().top`);
    check(`${look}: an agent shows above the fold on a phone`, firstRow < 844 * 0.6, String(firstRow));
    check(`${look}: nothing is wider than the phone`, !(await evaluate("document.documentElement.scrollWidth > window.innerWidth + 1")));
    n += 1;
    await picture(`${String(n).padStart(2, "0")}-${look}-phone`);
    await evaluate(`document.getElementById("board-open").click()`);
    await sleep(700);
    n += 1;
    await picture(`${String(n).padStart(2, "0")}-${look}-phone-board`);
    await stopServing();
  }
} catch (e) {
  console.log("  FAIL  " + e.message);
  failed += 1;
} finally {
  await cleanup();
}
console.log(failed ? `\n${failed} checks failed.` : "\nAll looks checks passed.");
process.exit(failed ? 1 : 0);
