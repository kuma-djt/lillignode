// In a real browser with no window: the page at phone width.
// The real Lillignode with two stand-in agents (one waiting on the
// reader) and the stand-in Board. The list of agents with state and age;
// a tap opens one agent's terminal and message box; the Board slides in
// from the right; who is blocking stays on top. No real agent.
//
//   node checks/page/phone-check.mjs [folder for pictures]
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
const pictures = resolve(process.argv[2] ?? join(here, "phone-out"));
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
const serving = spawn(join(engine, "target/release/examples/page_check" + ext), ["--serve", "--type"], {
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

  await width(390, 844);
  await until(text("#agents"), (t) => t !== null && t.includes("reporter") && t.includes("plain"), 30000);
  await tell("board standin", /DONE board/);
  await tell("key p", /DONE key/);
  const filmed = film(26000);

  console.log("A. At phone width: the list of agents, with state and age");
  const rows = await until(text("#agents"), (t) => t !== null && t.includes("Needs you"), 15000);
  check("each agent is listed with its state and age", rows !== null && /Needs you/.test(rows) && /Unknown|Idle/.test(rows) && /ago|waiting/.test(rows), rows);
  check("no terminal is shown on the list", !(await evaluate(visible("#terminal"))));
  check("the Board is not shown until asked for", !(await evaluate(visible("#board"))));
  const boardButton = await until(text("#board-open"), (t) => /^Board: \d+ to act on$/.test(t || ""), 8000);
  check("a Board button in the header says how many cards there are to act on", /^Board: \d+ to act on$/.test(boardButton || ""), boardButton);
  const blocked = await evaluate(text("#blocking"));
  check("who is blocking is on top", blocked !== null && blocked.includes("reporter") && blocked.includes("needs you"), blocked);
  const mark = await evaluate(`getComputedStyle(document.querySelector("#agents li button"), "::after").content`);
  check("each row ends with a mark that it opens its agent", mark === '"\u203A"', mark);
  await picture("01-phone-list");
  await sleep(1500);

  console.log("B. A tap opens one agent: its terminal and the message box");
  await evaluate(`[...document.querySelectorAll("#agents li")].find((li) => li.innerText.includes("plain")).querySelector("button").click()`);
  await sleep(600);
  const term = await until(text("#terminal"), (t) => (t || "").includes("STANDIN READY"), 8000);
  check("its terminal is shown, with its screen", (await evaluate(visible("#terminal"))) && (term || "").includes("STANDIN READY"), term);
  check("named", ((await evaluate(text("#terminal-title"))) || "").includes("plain"));
  check("with the message box", await evaluate(visible("#typing-text")));
  const boxTop = await evaluate(top("#typing-text"));
  const termTop = await evaluate(top("#terminal"));
  check("the message box, and whom it is to, come before the terminal, in sight", boxTop < termTop && boxTop < 844, `box ${boxTop}, terminal ${termTop}`);
  check("and a way back to all agents", ((await evaluate(text("#back-to-agents"))) || "").startsWith("\u2190 All agents"));
  check("the list is not shown behind it", !(await evaluate(visible("#agents"))));
  await picture("02-phone-agent");
  await sleep(1500);

  // Scrolled down in the agent's view, which can scroll: who is blocking
  // stays whole, just below the header.
  await evaluate("window.scrollTo(0, document.body.scrollHeight)");
  await sleep(500);
  const scrolledBy = await evaluate("window.scrollY");
  const pinned = await evaluate(top("#blocking"));
  const headerBottom = await evaluate(`document.querySelector("header").getBoundingClientRect().bottom`);
  check("the agent's view scrolls", scrolledBy > 20, String(scrolledBy));
  check("scrolled, who is blocking is still just below the header", pinned >= headerBottom - 1 && pinned <= headerBottom + 2, `top ${pinned}, header bottom ${headerBottom}`);
  const coveredBy = await evaluate(`(() => { const r = document.querySelector("#blocking-list li").getBoundingClientRect(); const e = document.elementFromPoint(r.left + 10, r.top + r.height / 2); return e ? e.closest("#blocking") ? "blocking" : e.tagName + "#" + e.id : "nothing"; })()`);
  check("and nothing covers it", coveredBy === "blocking", coveredBy);
  await picture("04-phone-scrolled");
  await sleep(1500);

  // The phone's own Back.
  await evaluate("history.back()");
  await sleep(800);
  check("the phone's own Back: the list again", (await evaluate(visible("#agents"))) && !(await evaluate(visible("#terminal"))));
  await sleep(1000);
  await evaluate(`[...document.querySelectorAll("#agents li")].find((li) => li.innerText.includes("plain")).querySelector("button").click()`);
  await sleep(800);
  await tap("#back-to-agents");
  await sleep(800);
  check("the All agents button: the list again", (await evaluate(visible("#agents"))) && !(await evaluate(visible("#terminal"))));
  await sleep(1000);

  console.log("C. The Board slides in from the right");
  await tap("#board-open");
  await sleep(700);
  check("the Board is shown", await evaluate(visible("#board")));
  const cards = await evaluate(text("#board"));
  check("with its cards", cards !== null && cards.includes("Decide"), cards);
  const left = await evaluate(`document.getElementById("board").getBoundingClientRect().left`);
  check("over the right of the screen, leaving a strip to tap", left > 0 && left < 100, String(left));
  const blockTop = await evaluate(`document.getElementById("blocking").getBoundingClientRect().bottom`);
  const boardTop = await evaluate(`document.getElementById("board").getBoundingClientRect().top`);
  check("who is blocking is still in sight above the open Board", boardTop >= blockTop - 1, `blocking ends ${blockTop}, Board starts ${boardTop}`);
  await picture("03-phone-board");
  check(
    "while it is open, nothing behind it takes keys, the header included",
    await evaluate(`["middle", "agents-column", "blocking"].every((id) => document.getElementById(id).inert) && document.querySelector("header").inert`),
  );
  await sleep(1500);
  await evaluate(`document.getElementById("board-shade").click()`);
  await sleep(700);
  check("a tap beside it closes it", !(await evaluate(visible("#board"))));

  console.log("D. A tap on who is blocking opens that agent");
  await sleep(1000);
  await evaluate(`document.querySelector("#blocking-list button").click()`);
  await sleep(600);
  check("a tap on it opens that agent", ((await evaluate(text("#terminal-title"))) || "").includes("reporter"));
  await filmed;

  console.log("E. At desktop width: as before");
  await width(1400, 900);
  check("no phone controls", !(await evaluate(visible("#board-open"))) && !(await evaluate(visible("#back-to-agents"))));
  check("agents, terminal and Board side by side", (await evaluate(visible("#agents"))) && (await evaluate(visible("#terminal"))) && (await evaluate(visible("#board"))));
  await picture("05-desktop");
  check("nothing is wider than the screen", !(await evaluate("document.documentElement.scrollWidth > window.innerWidth + 1")));
} catch (e) {
  console.log("  FAIL  " + e.message);
  failed += 1;
} finally {
  await cleanup();
}
console.log(failed ? `\n${failed} checks failed.` : "\nAll phone checks passed.");
process.exit(failed ? 1 : 0);
