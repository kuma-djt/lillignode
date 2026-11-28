// In a real browser with no window: saved teams on the page, and
// starting one from it with "page_can_start" on. The real Lillignode with
// stand-in agents and a stand-in teams.json. No real agent.
//
//   node checks/page/teams-check.mjs [folder for pictures]
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
const pictures = resolve(process.argv[2] ?? join(here, "teams-out"));
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
const serving = spawn(join(engine, "target/release/examples/page_check" + ext), ["--serve", "--act"], {
  stdio: ["pipe", "pipe", "inherit"],
  env: { ...process.env, PAGE_CHECK_SETTINGS: '"page_can_start": true' },
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
  await tell("teams", /DONE teams/);

  console.log("A. The saved teams, under the agents");
  const listed = await until(text("#teams"), (t) => t !== null && t.includes("docs"), 10000);
  check("each saved team is listed with what each agent runs, and where", listed !== null && listed.includes("web") && listed.includes("reporter in the folder web-api") && listed.includes("plain in the folder web-docs"), listed);
  const runs = await evaluate(`[...document.querySelectorAll("#teams .team-runs")].map((e) => e.textContent).join("|")`);
  check("each program is shown as it is run, in a typeface for commands", runs === "reporter|plain|plain", runs);
  const starts = await evaluate(`document.querySelectorAll("#teams .team-start").length`);
  check("each has a Start button, with starting on", starts === 2, String(starts));
  const standing = await evaluate(text("#standing"));
  check("the header says the page can start teams", (standing || "").includes("start your saved teams"), standing);
  await picture("01-teams");
  const filmed = film(24000);
  await sleep(1500);

  const saidOf = (team) => `(() => { const li = [...document.querySelectorAll("#teams .team-row")].find((r) => r.querySelector(".team-name").textContent === "${team}"); const p = li && li.querySelector(".team-said"); return p ? p.textContent : null; })()`;
  const rowText = (team) => `(() => { const li = [...document.querySelectorAll("#teams .team-row")].find((r) => r.querySelector(".team-name").textContent === "${team}"); return li ? li.textContent : null; })()`;

  console.log("B. Start asks first, under the team");
  check("before any start, the team says none of it is running", ((await evaluate(rowText("web"))) || "").includes("0 of 2 running"), await evaluate(rowText("web")));
  await evaluate(`document.querySelector('#teams .team-start[aria-label="Start the team web"]').click()`);
  await sleep(300);
  const asked = await evaluate(rowText("web"));
  check("one press of Start starts nothing: it asks, saying how many programs", (asked || "").includes("Start 2 programs as listed above, each in a new conversation?"), asked);
  const agentsBefore = await evaluate(text("#agents"));
  check("and nothing was started by it", !(agentsBefore || "").includes("web-api"), agentsBefore);
  await picture("02-start-asks");
  await sleep(2000);
  await evaluate(`document.querySelector('#teams .team-yes').click()`);
  const said = await until(saidOf("web"), (t) => t !== null && t.length > 0, 15000);
  check("Yes, start: the page says what was started, under that team, with the time", /^Started the team "web": reporter \(web-api\), plain \(web-docs\)\. In new conversations\. \(.+\)$/.test(said || ""), said);
  const agents = await until(text("#agents"), (t) => t !== null && t.includes("web-api") && t.includes("web-docs"), 15000);
  check("and the new agents are in the list", agents !== null && agents.includes("web-api") && agents.includes("web-docs"), agents);
  const running = await until(rowText("web"), (t) => (t || "").includes("running") && !(t || "").includes("of 2"), 10000);
  check("and the team says it is running", running !== null, await evaluate(rowText("web")));
  await picture("03-team-started");
  await sleep(2500);

  console.log("C. Again, Cancel, and a team whose folder is gone");
  await evaluate(`document.querySelector('#teams .team-start[aria-label="Start the team web"]').click()`);
  await sleep(300);
  const again = await evaluate(rowText("web"));
  const yesOff = await evaluate(`document.querySelector('#teams .team-yes').disabled`);
  check("pressed again, it says all of it is running and offers no Yes", (again || "").includes("All of it is running already: nothing would start.") && yesOff === true, again);
  await sleep(1500);
  await evaluate(`document.querySelector('#teams .team-no').click()`);
  await sleep(300);
  check("Cancel puts the Start button back", (await evaluate(`document.querySelectorAll("#teams .team-start").length`)) === 2);
  await evaluate(`document.querySelector('#teams .team-start[aria-label="Start the team docs"]').click()`);
  await sleep(1200);
  await evaluate(`document.querySelector('#teams .team-yes').click()`);
  const gone = await until(saidOf("docs"), (t) => (t || "").includes("Not started"), 15000);
  check("a team whose folder is gone says so, by name, under that team", (gone || "").includes("Not started: plain (gone): its folder"), gone);
  const webStill = await evaluate(saidOf("web"));
  // A start that went through is said for a few seconds, then goes, so
  // the team is as it was; it is never said under another team.
  check("what the other team's start came to is under that team, or gone after its few seconds", webStill === null || (webStill || "").startsWith('Started the team "web"'), webStill);
  await picture("04-folder-gone");
  await filmed;

  console.log("D. At phone width");
  await width(390, 844);
  await evaluate(`document.getElementById("teams").scrollIntoView()`);
  await picture("05-phone-teams");
  check("nothing is wider than the phone", !(await evaluate("document.documentElement.scrollWidth > window.innerWidth + 1")));
} catch (e) {
  console.log("  FAIL  " + e.message);
  failed += 1;
} finally {
  await cleanup();
}
console.log(failed ? `\n${failed} checks failed.` : "\nAll teams checks passed.");
process.exit(failed ? 1 : 0);
