// measure.mjs: where each page's answer to its real question sits in the first 1440x900 screen.
import { spawn } from "node:child_process";
const U = "http://127.0.0.1:47811/v5/page/Page.html";
const Q = [
  ["rs-value", "How do I get a toml::Value from a string?", ".rail.lead"],
  ["rs-datetime", "Which parts does a Local Date carry?", "#bracket [data-a=rung-own-0]"],
  ["rs-from_str", "What can fail when I call serde_json::from_str?", "#drops [data-a=drop-0]"],
  ["rs-serialize", "Which of my types implement Serialize?", "#outline"],
  ["ts-issue", "What does a too_small issue carry?", "[data-a=case-2]"],
  ["ts-toosmall", "Is inclusive always set?", "[data-a=rung-own-3]"],
  ["ts-parse", "What can fail when I call schema.parse(data)?", "#drops [data-a=drop-0]"],
  ["ts-collection", "What must I write to make my own collection node?", "[data-a=req-0]"],
  ["go-errorhandling", "Which setting makes Parse panic?", "[data-a=case-2]"],
  ["go-flag", "How do I get the Flag for --verbose?", ".rail.lead"],
  ["go-parse", "What can fail when I call FlagSet.Parse?", "#policy"],
  ["go-value", "Which types can I pass as a flag Value?", "#outline"],
  ["pkg-toml", "Where do I start reading toml?", "#tour"],
];
const chrome = spawn("/Applications/Google Chrome.app/Contents/MacOS/Google Chrome", ["--headless=new", "--remote-debugging-port=9338", "--disable-gpu", "--hide-scrollbars", "--user-data-dir=/tmp/v5-page-chrome-m", "about:blank"], { stdio: "ignore" });
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
let t = null; for (let i = 0; i < 100 && !t; i++) { try { t = await (await fetch("http://127.0.0.1:9338/json/list")).json(); } catch { await sleep(200); } }
const ws = new WebSocket(t.find((x) => x.type === "page").webSocketDebuggerUrl); await new Promise((r) => ws.addEventListener("open", r));
let id = 0; const w = new Map(); ws.addEventListener("message", (e) => { const m = JSON.parse(e.data); if (w.has(m.id)) { w.get(m.id)(m); w.delete(m.id); } });
const send = (method, params = {}) => new Promise((r) => { const k = ++id; w.set(k, r); ws.send(JSON.stringify({ id: k, method, params })); });
const ev = async (x) => (await send("Runtime.evaluate", { expression: x, returnByValue: true })).result.result.value;
await send("Network.enable"); await send("Network.setCacheDisabled", { cacheDisabled: true });
await send("Emulation.setDeviceMetricsOverride", { width: 1440, height: 900, deviceScaleFactor: 1, mobile: false });
for (const [p, q, sel] of Q) {
  await send("Page.navigate", { url: `${U}?p=${p}&w=1440&h=900` });
  for (let i = 0; i < 100; i++) { await sleep(50); if ((await ev("document.body && document.body.dataset.ready")) === "1") break; }
  const r = await ev(`(() => { const e = document.querySelector(${JSON.stringify(sel)}); if (!e) return null; const b = e.getBoundingClientRect(); return [Math.round(b.top), Math.round(b.bottom)]; })()`);
  console.log(`${p.padEnd(17)} ${r ? `y ${r[0]}–${r[1]}` : "missing"}  ${r && r[1] <= 874 ? "in the first screen" : "below the fold"}  | ${q}`);
}
ws.close(); chrome.kill();
