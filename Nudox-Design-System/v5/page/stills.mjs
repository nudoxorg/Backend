// stills.mjs: every design still, through one headless Chrome driven over CDP (no per-still launch).
//
//   node v5/page/stills.mjs [filter]          (needs the design server on :47811)
//
// Each still is { name, q, w, h? }: h omitted = the whole page; given = exactly that window.
import fs from "node:fs";
import path from "node:path";
import { spawn } from "node:child_process";

const OUT = process.env.OUT || "/private/tmp/claude-501/-Users-mileswirht-Downloads-backend/4821aaf6-28dc-4f75-b07b-d8fc41618656/scratchpad/wave5/page/stills";
const U = "http://127.0.0.1:47811/v5/page/Page.html";
const CHROME = "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome";
const PORT = 9337;
fs.mkdirSync(OUT, { recursive: true });

const PAGES = ["rs-value", "rs-datetime", "rs-from_str", "rs-serialize", "ts-issue", "ts-toosmall", "ts-parse", "ts-collection", "go-errorhandling", "go-flag", "go-parse", "go-value"];
const LIST = [];
for (const p of PAGES) { LIST.push({ name: `${p}-1440`, q: `p=${p}`, w: 1440 }); LIST.push({ name: `${p}-760`, q: `p=${p}`, w: 760 }); }
LIST.push({ name: "pkg-toml-1440", q: "p=pkg-toml", w: 1440 }, { name: "pkg-toml-760", q: "p=pkg-toml", w: 760 });
for (const p of ["rs-value", "ts-parse", "go-value", "pkg-toml", "go-parse", "ts-issue"]) LIST.push({ name: `${p}-1440-glacier`, q: `p=${p}&theme=glacier`, w: 1440 });
// the two directions for the specimen, same pages, the fold only
for (const p of ["rs-value", "ts-issue", "rs-from_str", "go-parse", "go-value", "ts-toosmall"]) { LIST.push({ name: `dirA-${p}`, q: `p=${p}`, w: 1440, h: 900 }); LIST.push({ name: `dirB-${p}`, q: `p=${p}&dir=b`, w: 1440, h: 900 }); }
// the fold: what you see without scrolling, per language
for (const p of [...PAGES, "pkg-toml"]) LIST.push({ name: `fold-${p}-1440`, q: `p=${p}`, w: 1440, h: 900 });
// hover and x-ray
LIST.push({ name: "hover-rs-value-table", q: "p=rs-value&hover=case:Table", w: 1440, h: 900 });
LIST.push({ name: "xray-rs-from_str", q: "p=rs-from_str&x=1", w: 1440, h: 900 });

const filter = process.argv[2] ? new RegExp(process.argv[2]) : null;
const todo = LIST.filter((s) => !filter || filter.test(s.name));

const chrome = spawn(CHROME, ["--headless=new", `--remote-debugging-port=${PORT}`, "--disable-gpu", "--hide-scrollbars", "--force-device-scale-factor=1", "--no-first-run", "--user-data-dir=/tmp/v5-page-chrome", "about:blank"], { stdio: "ignore" });
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
let targets = null;
for (let i = 0; i < 100 && !targets; i++) { try { targets = await (await fetch(`http://127.0.0.1:${PORT}/json/list`)).json(); } catch { await sleep(200); } }
const page = targets.find((t) => t.type === "page");
const ws = new WebSocket(page.webSocketDebuggerUrl);
await new Promise((r) => ws.addEventListener("open", r));
let id = 0; const waiting = new Map();
ws.addEventListener("message", (e) => { const m = JSON.parse(e.data); if (m.id && waiting.has(m.id)) { waiting.get(m.id)(m); waiting.delete(m.id); } });
const send = (method, params = {}) => new Promise((r) => { const k = ++id; waiting.set(k, r); ws.send(JSON.stringify({ id: k, method, params })); });
const evalJS = async (expr) => (await send("Runtime.evaluate", { expression: expr, returnByValue: true })).result.result.value;

await send("Page.enable"); await send("Network.enable"); await send("Network.setCacheDisabled", { cacheDisabled: true });
for (const s of todo) {
  const t0 = Date.now();
  await send("Emulation.setDeviceMetricsOverride", { width: s.w, height: s.h || 900, deviceScaleFactor: 1, mobile: false });
  const q = `${s.q}&w=${s.w}${s.h ? `&h=${s.h}` : "&tall=1"}`;
  await send("Page.navigate", { url: `${U}?${q}` });
  for (let i = 0; i < 200; i++) { await sleep(50); if ((await evalJS("document.body && document.body.dataset.ready")) === "1") break; }
  let h = s.h;
  if (!h) { h = await evalJS("Math.ceil(document.getElementById('win').getBoundingClientRect().height)"); await send("Emulation.setDeviceMetricsOverride", { width: s.w, height: h, deviceScaleFactor: 1, mobile: false }); await sleep(120); }
  const shot = await send("Page.captureScreenshot", { format: "png", clip: { x: 0, y: 0, width: s.w, height: h, scale: 1 }, captureBeyondViewport: true });
  fs.writeFileSync(path.join(OUT, `${s.name}.png`), Buffer.from(shot.result.data, "base64"));
  const errs = await evalJS("window.__errs || ''");
  console.log(`${s.name.padEnd(28)} ${s.w}x${h} ${Date.now() - t0}ms ${errs}`);
}
ws.close(); chrome.kill();
