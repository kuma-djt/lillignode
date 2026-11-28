// In a real browser with no window: agents' faces. The real Lillignode
// with three stand-in agents, "plain", "reporter" and "whole", and two
// pictures in the faces folder beside its workspace, for "plain" and
// "reporter": those two show their pictures in the round slot, "whole"
// keeps its initials. Once in each look; one picture of each.
//
//   node checks/page/faces-check.mjs [folder for pictures]
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
const pictures = resolve(process.argv[2] ?? join(here, "faces-out"));
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
    env: { ...process.env, PAGE_CHECK_SETTINGS: `"page_look": "${look}"`, PAGE_CHECK_THIRD: "whole" },
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
  for (const look of ["cards", "soft", "bold"]) {
    console.log(`\n${look}`);
    startServing(look);
    const ready = await line(/READY (http:\/\/127\.0\.0\.1:\d+\/[a-z0-9]{16}\/)/);
    await tell("faces", /DONE faces/);
    await tell("key w", /DONE key/);
    await width(1400, 900);
    await send("Page.navigate", { url: ready[1] });
    const faced = await until(
      `[...document.querySelectorAll("#agents li button")].map((b) => (b.innerText.split("\\n")[0]) + "=" + (b.dataset.face || "")).join(" | ")`,
      (t) => /plain[^|]*=plain/.test(t || "") && /reporter[^|]*=reporter/.test(t || ""),
      20000,
    );
    check(`${look}: plain and reporter are given their pictures, whole none`, /plain[^|]*=plain/.test(faced || "") && /reporter[^|]*=reporter/.test(faced || "") && /whole[^|]*=( |$)/.test((faced || "") + " "), faced);
    const slots = await evaluate(`[...document.querySelectorAll("#agents li button")].map((b) => { const s = getComputedStyle(b, "::before"); return { name: b.innerText.split("\\n")[0], image: s.backgroundImage.startsWith("url(") , content: s.content }; })`);
    const byName = (n) => slots.find((x) => x.name.startsWith(n)) || {};
    check(`${look}: the two pictures are in their round slots`, byName("plain").image && byName("reporter").image, JSON.stringify(slots));
    check(`${look}: whole keeps its initials`, !byName("whole").image && /"WH"/.test(byName("whole").content || ""), JSON.stringify(slots));
    const loaded = await evaluate(`Promise.all(["plain", "reporter"].map((f) => new Promise((done) => { const i = new Image(); i.onload = () => done(i.naturalWidth); i.onerror = () => done(0); i.src = "face/" + f; })))`);
    check(`${look}: both pictures load from the page's own address`, loaded.every((w) => w > 0), JSON.stringify(loaded));
    if (look === "cards") {
      const head = await evaluate(`fetch("face/plain").then((r) => ({ status: r.status, type: r.headers.get("content-type"), sniff: r.headers.get("x-content-type-options") }))`);
      check("a picture is said to be what it is, and the browser told not to guess", head.status === 200 && head.type === "image/png" && head.sniff === "nosniff", JSON.stringify(head));
      const outside = await evaluate(`Promise.all(["face/..%2Fconfig.json", "face/whole", "face/"].map((p) => fetch(p).then((r) => r.status)))`);
      check("nothing else is served as a picture", outside.every((s) => s === 404), JSON.stringify(outside));
    }
    await sleep(800);
    n += 1;
    await picture(`${String(n).padStart(2, "0")}-${look}-faces`);
    await stopServing();
  }
} catch (e) {
  console.log("  FAIL  " + e.message);
  failed += 1;
} finally {
  await cleanup();
}
console.log(failed ? `\n${failed} checks failed.` : "\nAll faces checks passed.");
process.exit(failed ? 1 : 0);
