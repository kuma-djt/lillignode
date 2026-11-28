// In a real browser with no window: the page's design pass. The real
// Lillignode with typing, acting and starting teams on, a stand-in Board
// adapter (which refuses card c5), a stand-in agent and two saved teams.
// Numbered pictures at 1400 and 390 wide, and a check of each point:
//   1. the header: the name, one status dot with last heard, the Board
//      count; what the page can do in a note on the first visit and
//      behind "?";
//   2. a phone: a tab bar (Agents, Board with its count); inside an agent
//      the terminal above and the message box at the bottom;
//   3. a refusal on the card itself, once, with what to do now;
//   4. Undo counting down on its button, the card dimmed until sent;
//   5. what was said for one agent not shown on another's view; the team
//      start question closed after a start;
//   6. a team's folder by its name, not its full path.
//
//   node checks/page/design-check.mjs [folder for pictures]
//   (LILLIGNODE_ENGINE names another engine checkout to use)
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
const pictures = resolve(process.argv[2] ?? join(here, "design-out"));
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
const scratch = mkdtempSync(join(tmpdir(), "lillignode-design-"));
let out = "";
const waiters = [];
const serving = spawn(join(engine, "target/release/examples/page_check" + ext), ["--serve", "--type", "--act"], {
  stdio: ["pipe", "pipe", "inherit"],
  env: {
    ...process.env,
    PAGE_CHECK_BOARD_ADAPTER: JSON.stringify({
      command: join(engine, "target/release/examples/adapter_standin" + ext),
      args: [join(engine, "examples/board-standin.json"), join(scratch, "adapter.log")],
      env: [],
    }),
    PAGE_CHECK_SETTINGS: '"page_can_start": true',
  },
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
  try {
    serving.stdin.write("quit\n");
    await Promise.race([line(/DONE quit/, 20000), sleep(20000)]);
  } catch {}
  serving.kill();
  browser.kill();
  await sleep(500);
  for (const dir of [profile, scratch]) {
    try {
      rmSync(dir, { recursive: true, force: true });
    } catch {}
  }
}

let n = 0;
const shot = async (name) => {
  n += 1;
  await picture(`${String(n).padStart(2, "0")}-${name}`);
};

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

  const ready = await line(/READY (http:\/\/127\.0\.0\.1:\d+\/[a-z0-9]{16}\/)/);
  await tell("teams", /DONE teams/);
  await tell("key p", /DONE key/);

  console.log("1. The header, at 1400 wide");
  await width(1400, 900);
  await send("Page.navigate", { url: ready[1] });
  await until(cardText("c5"), (t) => t !== null, 20000);
  await until(`(document.getElementById("agents") || {}).innerText`, (t) => /Needs you/i.test(t || ""), 20000);
  await sleep(800);
  await shot("desktop-first-visit");
  check("the first visit shows what the page can do, as a note", await evaluate(visible("#about")));
  await evaluate(`(() => { const b = document.getElementById("about-close"); if (b) b.click(); })()`);
  await sleep(300);
  const header = await evaluate(`(() => { const h = document.querySelector("header"); return { height: Math.round(h.getBoundingClientRect().height), text: h.innerText.replace(/\\s+/g, " ") }; })()`);
  check(
    "the header is the name, a dot with last heard, and the Board count",
    /Lillignode/.test(header.text) && /\d+ s ago/.test(header.text) && /\d+ to act on/.test(header.text) && !/This page can/.test(header.text) && header.height <= 72,
    JSON.stringify(header),
  );
  check("there is one status dot", (await evaluate(`document.querySelectorAll("header .dot").length`)) === 1);
  check(
    "a ? shows the note again, and closes it",
    await evaluate(`(() => { const q = document.getElementById("about-open"); if (!q) return false; q.click(); const shown = !document.getElementById("about").hidden; q.click(); return shown && document.getElementById("about").hidden; })()`),
  );
  await shot("desktop");

  console.log("4. Undo counts down, and the card is dimmed until it is sent");
  await evaluate(press('#board li[data-id="c1"]', "/^Approve$/"));
  await sleep(2200);
  const undo = await evaluate(`(() => { const li = document.querySelector('#board li[data-id="c1"]'); const b = li && [...li.querySelectorAll("button")].find((x) => /Undo/.test(x.textContent)); const seen = (e) => { let o = 1; for (; e && e !== document.body; e = e.parentElement) o *= Number(getComputedStyle(e).opacity); return o; }; return { button: b ? b.textContent.trim() : null, title: li ? seen(li.querySelector(".card-title")) : 1, undo: b ? seen(b) : 0, count: (document.getElementById("board-count") || {}).textContent }; })()`);
  check("the Undo button counts down", /^Undo \(\d+ s\)$/.test(undo.button || ""), JSON.stringify(undo));
  check("the card is dimmed while it can be undone, its Undo button not", undo.title < 0.9 && undo.undo === 1, JSON.stringify(undo));
  check("the count no longer counts it", undo.count === "5 to act on", JSON.stringify(undo));
  await shot("desktop-undo-counting-down");
  await until(cardText("c1"), (t) => t === null || /Taken by/.test(t), 20000);

  console.log("3. A refusal shows on the card, once");
  await evaluate(press('#board li[data-id="c5"]', "/Done/"));
  const refused = await until(cardText("c5"), (t) => /Refused by/.test(t || ""), 25000);
  check(
    "the refusal is on the card, in the Hub's words",
    /Refused by the stand-in Hub/.test(refused || "") && /that card was closed at the service/.test(refused || ""),
    refused,
  );
  check(
    "and says what to do now: try again, open it on the Hub, or dismiss",
    /Try again/.test(refused || "") && /Dismiss/.test(refused || "") && /open it on the stand-in Hub/i.test(refused || ""),
    refused,
  );
  await sleep(1500);
  const times = await evaluate(saying("that card was closed at the service"));
  check("the Hub's words are said once on the page", times === 1, String(times));
  await evaluate(`document.querySelector('#board li[data-id="c5"]').scrollIntoView({ block: "center" })`);
  await sleep(300);
  await shot("desktop-refused-on-the-card");
  await evaluate(press('#board li[data-id="c5"]', "/^Dismiss$/"));
  await sleep(600);
  check("Dismiss takes it away", !/Refused by/.test((await evaluate(cardText("c5"))) || ""));

  console.log("5 and 6. Teams: the folder by name, and the question closes after a start");
  await evaluate(`document.getElementById("teams").scrollIntoView()`);
  const folders = await evaluate(`[...document.querySelectorAll(".team-agent")].map((e) => e.innerText)`);
  check(
    "a team's folders are named, not given as full paths",
    folders.length > 0 && folders.every((f) => !/[A-Za-z]:\\|\//.test(f.split("in the folder").pop())),
    JSON.stringify(folders),
  );
  await evaluate(press(".team-row", "/^Start$/"));
  await sleep(400);
  await shot("desktop-team-asks");
  await evaluate(press(".team-row", "/^Yes, start$/"));
  await until(`[...document.querySelectorAll(".team-said")].map((e) => e.innerText).join(" ")`, (t) => /Started|Already/.test(t || ""), 15000);
  await sleep(2500);
  check("after a start the question is closed", !(await evaluate(visible(".team-ask"))));
  await shot("desktop-team-started");

  console.log("5. What was said for one agent is not shown on another's");
  await evaluate(`[...document.querySelectorAll("#agents li")].find((li) => /reporter/.test(li.innerText)).firstChild.click()`);
  await sleep(500);
  await evaluate(`document.getElementById("typing-enter").click()`);
  const saidThere = await until(text("#typing-said"), (t) => /Enter|Sent/.test(t || ""), 10000);
  check("Press Enter says what came of it, under that agent", /Enter|Sent/.test(saidThere || ""), saidThere);
  await evaluate(`[...document.querySelectorAll("#agents li")].find((li) => !/reporter/.test(li.innerText.split("\\n")[0])).firstChild.click()`);
  await sleep(600);
  const elsewhere = await evaluate(text("#typing-said"));
  check("another agent's view does not show it", !elsewhere, elsewhere);
  await shot("desktop-another-agent");

  console.log("2. A phone");
  await width(390, 844);
  await send("Page.navigate", { url: ready[1] });
  await until(`(document.getElementById("agents") || {}).innerText`, (t) => (t || "").length > 10, 20000);
  await sleep(1500);
  await shot("phone-agents");
  const phoneHeader = await evaluate(`Math.round(document.querySelector("header").getBoundingClientRect().height)`);
  check("the phone's header is one short row", phoneHeader <= 64, String(phoneHeader));
  const tabs = await evaluate(`(() => { const t = document.getElementById("tabs"); if (!t || t.closest("[hidden]") || !t.getClientRects().length) return null; const r = t.getBoundingClientRect(); const b = document.querySelector("#tabs .badge"); return { bottom: Math.round(r.bottom), text: t.innerText.replace(/\\s+/g, " "), badge: b ? b.textContent : null, bg: b ? getComputedStyle(b).backgroundColor : null }; })()`);
  check(
    "a tab bar at the bottom: Agents, and Board with its count in orange",
    !!tabs && tabs.bottom >= 840 && /Agents/.test(tabs.text) && /Board/.test(tabs.text) && /^\d+$/.test(tabs.badge || "") && tabs.bg === "rgb(227, 154, 85)",
    JSON.stringify(tabs),
  );
  check("nothing is wider than the phone", !(await evaluate("document.documentElement.scrollWidth > window.innerWidth + 1")));
  await evaluate(`document.querySelector("#agents li button").click()`);
  await sleep(900);
  await shot("phone-agent");
  const chat = await evaluate(`(() => { const t = document.getElementById("terminal").getBoundingClientRect(); const b = document.getElementById("typing").getBoundingClientRect(); return { terminalTop: Math.round(t.top), boxTop: Math.round(b.top), boxBottom: Math.round(b.bottom) }; })()`);
  check(
    "inside an agent: the terminal above, the message box at the bottom",
    chat.terminalTop < chat.boxTop && chat.boxBottom >= 840 && chat.boxBottom <= 845,
    JSON.stringify(chat),
  );
  // Its first line can be seen: nothing pinned covers it.
  const firstLine = await evaluate(`(() => { const l = document.querySelector("#terminal > span"); if (!l) return "no line"; const r = l.getBoundingClientRect(); const e = document.elementFromPoint(r.left + 4, r.top + r.height / 2); return e && e.closest("#terminal") ? "seen" : (e ? e.tagName + "#" + e.id : "nothing"); })()`);
  check("and the terminal's first line is in sight, not under a bar", firstLine === "seen", firstLine);
  await evaluate(`document.getElementById("back-to-agents").click()`);
  await sleep(500);
  await evaluate(`(document.getElementById("tab-board") || document.getElementById("board-open")).click()`);
  await sleep(900);
  await shot("phone-board");
  check("the Board tab shows the Board", await evaluate(visible("#board-decide")));
} catch (e) {
  console.log("  FAIL  " + e.message);
  failed += 1;
} finally {
  await cleanup();
}
console.log(failed ? `\n${failed} checks failed.` : "\nAll design checks passed.");
process.exit(failed ? 1 : 0);
