// In a real browser with no window: the real Lillignode with the
// page on and nothing else: no agent, no Board, no settings but the page.
// What a person sees first.
//
//   node checks/page/first-run-check.mjs [folder for pictures]
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
const pictures = resolve(process.argv[2] ?? join(here, "first-run-out"));
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

// --- the real Lillignode, serving the page, with the adapter named ------
const ext = process.platform === "win32" ? ".exe" : "";
let out = "";
const waiters = [];
const serving = spawn(join(engine, "target/release/examples/page_check" + ext), ["--serve", "--empty"], {
  stdio: ["pipe", "pipe", "inherit"],
});
serving.stdout.on("data", (d) => {
  out += d;
  for (const w of [...waiters]) w();
});
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

  const visible = (sel) => `(() => { const e = document.querySelector(${JSON.stringify(sel)}); return !!e && !e.closest("[hidden]") && e.getBoundingClientRect().height > 0; })()`;

  console.log("A. Nothing yet: what Lillignode is, and the one next step");
  const first = await until(text("#first-run"), (t) => t !== null);
  check("the page says nothing is running yet", first !== null && first.includes("Nothing is running yet"), first);
  check("it says what Lillignode is", first !== null && first.includes("runs AI coding agents side by side"), first);
  check(
    "and the one next step, in Lillignode's own window",
    first !== null && first.includes("Next: start your first agent in Lillignode's own window.") && first.includes("Press n there"),
    first,
  );
  check("no empty terminal is drawn", !(await evaluate(visible("#terminal"))));
  check("the agents column says there are none", (await evaluate(text("#no-agents"))) === "No agents are open in Lillignode.");
  const empty = await until(text("#board-note"), (t) => t !== null && t.includes("board.json"));
  check(
    "the empty Board says where cards would come from",
    empty !== null && empty.includes("from a file you keep (board.json") && empty.includes("lillignode --where"),
    empty,
  );
  check("the header says the page is read only", ((await evaluate(text("#standing"))) || "").startsWith("Read only."));
  await picture("01-first-run");

  console.log("B. At phone width");
  await width(390, 844);
  await evaluate("window.scrollTo(0, 0)");
  await picture("02-phone-first-run");
  check("nothing is wider than the phone", !(await evaluate("document.documentElement.scrollWidth > window.innerWidth + 1")));

  console.log("C. Lillignode gone");
  await width(1400, 900);
  serving.stdin.write("kill\n");
  const gone = await until(visible("#first-run"), (v) => v === false, 30000);
  check("the panel's promise is taken away once Lillignode cannot be heard", gone === false);
  check("and the page says contact is lost", ((await evaluate(text("#contact"))) || "").includes("Lost contact with Lillignode"));
} catch (e) {
  console.log("  FAIL  " + e.message);
  failed += 1;
} finally {
  await cleanup();
}
console.log(failed ? `\n${failed} checks failed.` : "\nAll first-run checks passed.");
process.exit(failed ? 1 : 0);
