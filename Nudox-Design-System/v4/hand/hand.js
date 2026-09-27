// Hand.html: the trail replaced by where you are (the jump bar) and what you hold (the hand).
//
// The window is the real neighbouring board, loaded same-origin and calmed in place (never edited):
//   host=page2  D-Page's toml::Value page (default)      host=browse  D-Browse's find page
//   host=orbit  the Orbit board (resume)
// Its titlebar's trail (beads, strands, the trail button) is replaced by the jump bar; the foot
// gets the hand. Names and chains come from graph/world.js and graph/recipes.js through
// hand-core.js; nothing here is invented.
//
// ?state= rest | siblings | members | keys | cmd | back | first | hand1 | hand3 | hand5 | hover |
//          code | drop | rest5 | seam | dock | find | orbit
// ?w= ?h= window size · ?text=200 text at 200 % · ?t= ms into a motion (first, drop) · ?play=1 plays it
(async () => {
  "use strict";
  const Q = new URLSearchParams(location.search);
  const STATE = Q.get("state") || "rest";
  const W = +(Q.get("w") || (STATE === "dock" ? 2560 : 1440));
  const H = +(Q.get("h") || (STATE === "dock" ? 1440 : 900));
  const T2 = Q.get("text") === "200";
  const PLAY = Q.get("play") === "1";
  const HOST = Q.get("host") || (STATE === "orbit" ? "orbit" : STATE === "find" ? "browse" : "page2");
  const esc = (s) => String(s).replace(/[&<>"]/g, (c) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;" })[c]);
  const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

  // ------------------------------------------------------------------ the world and the chain finder
  const WD = window.WORLD, N = WD.nodes, PK = WD.packages, MD = WD.modules;
  const kids = Array.from({ length: N.length }, () => null);
  for (let i = 0; i < N.length; i++) { const u = N[i].u; if (u >= 0) (kids[u] ||= []).push(i); }
  const topOf = new Int32Array(N.length); for (let i = 0; i < N.length; i++) { let j = i; while (N[j].u >= 0) j = N[j].u; topOf[i] = j; }
  const G = { N, PK, MD, kids, topOf, IMP: Float32Array.from(WD.imp), yours: (i) => PK[N[i].p].yours, pageOpen: false, focus: -1 };
  window.GRAPH_PAGE.init(G);
  const R = window.GRAPH_RECIPES, HC = window.HAND_CORE;
  HC.init(G, R, window.GRAPH_PAGE.types);
  const pkName = (p) => PK[p].name.replace(/^backend-/, "");
  const node = (pkg, mod, name, kind) => N.findIndex((n) => PK[n.p].name === pkg && MD[n.m].path === mod && n.n === name && n.u < 0 && (!kind || n.k === kind) && !n.orphan);
  const ID = {
    value: node("toml", "value", "Value", "enum"), table: node("toml", "table", "Table", "type"), from: node("toml", "de", "from_str", "function"),
    to: node("toml", "ser", "to_string", "function"), de: node("serde_core", "de", "Deserialize", "trait"), map: node("toml", "map", "Map", "struct"),
    label: node("backend-present", "glyph", "RelationLabel", "enum"), dir: node("backend-present", "glyph", "RelationDirection", "enum"),
  };
  const HERE = ID.value;

  // ------------------------------------------------------------------ house glyphs
  const ICONS = {};
  for (const it of await (await fetch("../../spec/icons.json")).json()) ICONS[it.group + "/" + it.name] = it.svg;
  const inner = (svg) => svg.slice(svg.indexOf(">") + 1, svg.lastIndexOf("</svg>"));
  const ico = (name, cls = "s14") => `<svg class="ico ${cls}" viewBox="0 0 24 24" aria-hidden="true">${inner(ICONS["ui/" + name] || ICONS["core/" + name])}</svg>`;
  const chev = (rot = 0) => `<svg class="ico" viewBox="0 0 24 24" aria-hidden="true"${rot ? ` style="transform:rotate(${rot}deg)"` : ""}><path d="m9 6 6 6-6 6"></path></svg>`;
  const FAM = { module: "ns", package: "ns", struct: "ty", class: "ty", enum: "ty", union: "ty", type: "ty", trait: "co", interface: "co", function: "ca", method: "ca", constructor: "ca", macro: "ca", constant: "va", field: "va", variant: "va" };
  const kmark = (k, size = "sm") => (k === "orbit" ? `<span class="k ns ${size}">${ico("orbit")}</span>` : `<span class="k ${FAM[k] || "ns"} ${size}"><svg viewBox="0 0 24 24" aria-label="${k}">${inner(ICONS["kind/" + k] || ICONS["kind/unknown"])}</svg></span>`);
  const LIT = [0.4, 0.3, 0.34, 0.14, 0.08, 0.11, 0.22, 0.2, 0.3, 0.56, 0.5, 0.66];
  const FACETS = ["24,2 35,13 24,10", "24,10 35,13 38,24", "35,13 46,24 38,24", "46,24 35,35 38,24", "38,24 35,35 24,38", "35,35 24,46 24,38", "24,46 13,35 24,38", "24,38 13,35 10,24", "13,35 2,24 10,24", "2,24 13,13 10,24", "10,24 13,13 24,10", "13,13 24,2 24,10"];
  function gem(k, px, light = 1) {
    const g = inner(ICONS["kind/" + k]).replace(/class="f"/g, 'class="f" fill="currentColor" stroke="none" opacity=".5"');
    return `<svg class="gem ${FAM[k] || "ns"}" width="${px}" height="${px}" viewBox="0 0 48 48">${FACETS.map((p, i) => `<polygon points="${p}" fill="currentColor" opacity="${(LIT[i] * light).toFixed(3)}"></polygon>`).join("")}`
      + `<path d="M24 2 46 24 24 46 2 24z" fill="none" stroke="currentColor" stroke-width="${1 + (1 - light) * 2}"></path>`
      + `<path d="M24 10 38 24 24 38 10 24z" fill="var(--table)" stroke="currentColor" stroke-width=".7" stroke-opacity="${(.55 * light).toFixed(2)}"></path>`
      + `<g transform="translate(17.4 17.4) scale(.55)" fill="none" stroke="currentColor" stroke-width="${2.4 + (1 - light) * 2}" stroke-linecap="square" stroke-linejoin="miter">${g}</g></svg>`;
  }

  // ------------------------------------------------------------------ the story: places and what you hold
  const where = (i) => { const n = N[i]; const m = MD[n.m].path; return pkName(n.p) + (m ? "::" + m : ""); };
  const place = (i) => ({ k: N[i].k, n: N[i].n, w: pkName(N[i].p) });
  const BACK = [place(ID.table), place(ID.map), place(ID.de), place(ID.from), { k: "package", n: "toml", w: "crates.io" }, place(ID.dir), place(ID.label),
    { k: "module", n: "glyph", w: "present" }, { k: "package", n: "present", w: "backend" }, { k: "orbit", n: "Orbit", w: "" }];
  const HELD = {
    value: { i: ID.value, why: "you pinned it", ago: "50 min ago" },
    table: { i: ID.table, why: "you copied its signature", ago: "12 min ago" },
    from: { i: ID.from, why: "you pinned it", ago: "3 min ago" },
    de: { i: ID.de, why: "you pinned it", ago: "yesterday", hollow: true },
    to: { i: ID.to, why: "you copied its signature", ago: "a minute ago" },
  };
  const HANDS = { 1: ["value"], 3: ["value", "table", "from"], 5: ["value", "table", "from", "de", "to"] };
  const keyOf = (i) => Object.keys(HELD).find((k) => HELD[k].i === i);

  // the hand, arranged: runs by what feeds what, then what stands apart
  function arrange(keys) {
    const ids = keys.map((k) => HELD[k].i);
    const twins = new Set(); const seen = new Map();
    for (const i of ids) { const n = N[i].n; if (seen.has(n) && seen.get(n) !== N[i].p) twins.add(n); seen.set(n, N[i].p); }
    const a = HC.arrange(ids);
    const order = []; // [{ i, j (join into it) , apart }]
    a.runs.forEach((run, r) => run.forEach((x, k) => order.push({ i: x.p.i, j: x.j, gap: r > 0 && k === 0 })));
    a.apart.forEach((p) => order.push({ i: p.i, j: null, gap: order.length > 0, apart: true }));
    const main = a.runs[0] || null;
    return { order, twins, main, say: main ? HC.sentence(main, twins) : "", code: main ? HC.code(main, twins) : "" };
  }

  // ------------------------------------------------------------------ the host board, loaded and calmed in place
  const stage = document.getElementById("stage");
  stage.style.width = W + "px"; stage.style.height = H + "px";
  const frame = document.createElement("iframe");
  frame.width = W; frame.height = H; frame.setAttribute("frameborder", "0");
  const hostURL = {
    page2: `../page2/Page2.html?page=value${T2 ? "&t=2" : ""}`,
    browse: `../browse/Browse.html?q=parse%20toml&hover=toml_edit&hand=toml,toml_edit,basic-toml${T2 ? "&zoom=2" : ""}`,
    orbit: W <= 800 ? "../Orbit4-760.html" : "../Orbit4.html",
  }[HOST] || HOST; // any other board path: its titlebar is calmed from its own capsule
  const GENERIC = !["page2", "browse", "orbit"].includes(HOST);
  frame.src = hostURL;
  stage.appendChild(frame);
  await new Promise((r) => frame.addEventListener("load", r, { once: true }));
  const d = frame.contentDocument, fw = frame.contentWindow;
  for (let k = 0; k < 120; k++) {
    const ok = HOST === "page2" ? d.body && d.body.dataset.h : HOST === "browse" ? d.querySelector(".win .titlebar") && d.querySelector(".handwrap, .bq") : d.querySelector(".win .titlebar");
    if (ok) break; await sleep(50);
  }
  await sleep(GENERIC ? 600 : 60);
  const link = d.createElement("link"); link.rel = "stylesheet"; link.href = new URL("hand.css", location.href).href; d.head.appendChild(link);
  await new Promise((r) => { link.onload = r; link.onerror = r; });
  const win = d.querySelector(".win");
  const Z = parseFloat(fw.getComputedStyle(d.documentElement).zoom) || 1;
  const rel = (el) => { const a = el.getBoundingClientRect(), b = win.getBoundingClientRect(); return { x: (a.left - b.left) / Z, y: (a.top - b.top) / Z, w: a.width / Z, h: a.height / Z, r: (a.right - b.left) / Z, b: (a.bottom - b.top) / Z }; };
  const WIN = () => rel(win);
  const q = (s) => win.querySelector(s);
  const html = (s) => { const t = d.createElement("template"); t.innerHTML = s.trim(); return t.content.firstElementChild; };
  const cmdHeld = STATE === "cmd";
  if (cmdHeld) d.body.classList.add("cmd");

  // ------------------------------------------------------------------ the jump bar
  const cap = (text, pos = "b") => `<kbd class="hcap c${pos}">${esc(text)}</kbd>`;
  function jumpHTML(o = {}) {
    const typing = o.typing != null;
    return `<div class="hctr">
      <button class="hnav hback${o.back ? " on" : ""}" aria-label="Back" title="Back · hold for recent places">${chev(180)}</button>
      <div class="hjump${o.focus ? " focus" : ""}" role="navigation" aria-label="Where you are">
        <span class="hsegs">
          <button class="hseg hs-pkg${o.open === "pkg" ? " open" : ""}" data-seg="pkg">toml</button><i class="hsep">${chev()}</i>
          <button class="hseg hs-mod${o.open === "mod" ? " open" : ""}${typing ? " typing" : ""}" data-seg="mod">${typing ? `${esc(o.typing)}<i class="caret"></i>` : "value"}</button><i class="hsep">${chev()}</i>
          <button class="hseg hs-item now${o.open === "item" ? " open" : ""}" data-seg="item">${kmark("enum")}<b>Value</b></button>
          <button class="hseg hs-deep${o.open === "deeper" ? " open show" : ""}" data-seg="deeper" aria-label="Members">${chev()}</button>
        </span>
        <button class="hfind" aria-label="Ask">${ico("search")}</button>
      </div>
    </div>`;
  }
  function calmTitlebar() {
    const tb = q(".titlebar"); if (!tb) return;
    const trail = tb.querySelector('[aria-label="Trail map"]'); if (trail) trail.remove();
    const th = tb.querySelector(".thread"); if (!th) return;
    if (HOST === "page2") { th.classList.add("hthread"); th.innerHTML = jumpHTML(JUMP); }
    else if (GENERIC && th.querySelector(".here:not(.askf)")) {
      // the board's own capsule says where it is: `RelationLabel` + `present › glyph`
      const here = th.querySelector(".here"), k = here.querySelector(".k"), nm = (here.querySelector(".nm") || {}).textContent || "";
      const path = ((here.querySelector(".path") || {}).textContent || "").split("›").map((x) => x.trim()).filter(Boolean);
      const segs = path.map((x) => `<button class="hseg">${esc(x)}</button><i class="hsep">${chev()}</i>`).join("");
      th.classList.add("hthread");
      th.innerHTML = `<div class="hctr"><button class="hnav hback" aria-label="Back">${chev(180)}</button><div class="hjump"><span class="hsegs">${segs}<button class="hseg now">${k ? k.outerHTML : ""}<b>${esc(nm)}</b></button><button class="hseg hs-deep">${chev()}</button></span><button class="hfind" aria-label="Ask">${ico("search")}</button></div></div>`;
    }
    else if (!th.querySelector(".hback")) { th.classList.add("hthread"); th.insertAdjacentHTML("afterbegin", `<button class="hnav hback" aria-label="Back">${chev(180)}</button>`); }
  }
  const JUMP = {};

  // ------------------------------------------------------------------ key caps (only while ⌘ is held), placed where their targets are painted
  function caps() {
    win.querySelectorAll(".hcaps").forEach((c) => c.remove());
    const box = html(`<div class="hcaps"></div>`); win.appendChild(box);
    const put = (el, text, where = "below") => {
      if (!el) return; const r = rel(el); if (!r.w) return;
      const x = where === "right" ? r.r + 12 : r.x + r.w / 2, y = where === "above" ? r.y - 20 : where === "right" ? r.y + r.h / 2 - 8 : r.b + 4;
      box.insertAdjacentHTML("beforeend", `<kbd class="hcap" style="left:${x.toFixed(1)}px;top:${y.toFixed(1)}px">${esc(text)}</kbd>`);
    };
    put(q(".hback"), "⌘[");
    put(q(".hjump"), "⌘L");
    put(q(".hfind"), "⌘K");
    // the hand: a digit over each mark (⌘ is already held), and H to open it
    win.querySelectorAll(".hmarks .hm").forEach((m, k) => { if (k < 5) put(m, String(k + 1), "above"); });
    win.querySelectorAll(".hhandw .hc").forEach((c, k) => { if (k < 5) put(c, "⌘" + (k + 1), "above"); });
    const ms = win.querySelectorAll(".hmarks .hm"); if (ms.length) put(ms[ms.length - 1], "H", "right");
  }

  // ------------------------------------------------------------------ menus
  const layer = html(`<div class="hlayer${["first", "drop"].includes(STATE) ? " hdriven" : ""}"></div>`); win.appendChild(layer);
  if (["first", "drop"].includes(STATE) && q(".cstatus")) q(".cstatus").classList.add("hdriven");
  const moduleRows = () => {
    const p = PK.findIndex((x) => x.name === "toml");
    const tops = [...new Set(MD.filter((m) => m.pkg === p && m.path).map((m) => m.path.split("::")[0]))].sort();
    return tops.map((m) => ({ k: "module", n: m, cur: m === "value", sub: MD.some((x) => x.pkg === p && x.path.startsWith(m + "::")) }));
  };
  const itemRows = () => N.map((n, i) => [n, i]).filter(([n]) => n.m === N[HERE].m && n.u < 0 && !n.orphan && n.k !== "import" && n.k !== "module")
    .sort((a, b) => a[0].l - b[0].l).map(([n, i]) => ({ k: n.k, n: n.n, cur: i === HERE }));
  const memberRows = () => {
    const ks = (kids[HERE] || []).map((j) => N[j]);
    const vs = ks.filter((n) => n.k === "variant").map((n) => ({ k: "variant", n: n.n }));
    const ms = ks.filter((n) => n.k === "method" && !n.via && n.v === "pub").map((n) => ({ k: "method", n: n.n }));
    return { vs, ms };
  };
  const packageRows = () => {
    const yours = PK.filter((p) => p.yours).map((p) => ({ k: "package", n: pkName(PK.indexOf(p)), yours: true }));
    const ext = PK.filter((p) => p.external).map((p) => ({ k: "package", n: p.name, cur: p.name === "toml" })).sort((a, b) => a.n.localeCompare(b.n));
    return { yours, ext };
  };
  const row = (r, extra = "") => `<div class="hmr${r.cur ? " cur" : ""}${r.sel ? " sel" : ""}${r.more ? " more" : ""}">${r.more ? "" : kmark(r.k)}<span class="nm">${r.html || esc(r.n)}</span>${r.w ? `<span class="w">${esc(r.w)}</span>` : ""}${r.sub ? `<span class="hsub">${chev()}</span>` : ""}${extra}</div>`;
  function menu(anchor, rowsHTML, { dx = 0, cls = "" } = {}) {
    const a = rel(anchor);
    const m = html(`<div class="hmenuw unfurl" style="left:${Math.round(a.x + dx)}px;top:${Math.round(a.b + 6)}px"><div class="hmenu ${cls}">${rowsHTML}</div></div>`);
    layer.appendChild(m);
    // keep inside the window
    const r = rel(m), wr = WIN();
    if (r.r > wr.w - 8) m.style.left = Math.max(8, wr.w - 8 - r.w) + "px";
    return m;
  }
  function openSeg(seg, o = {}) {
    closeMenus();
    const el = q(`.hseg[data-seg="${seg}"]`); if (!el) return;
    q(".hjump").querySelectorAll(".hseg").forEach((s) => s.classList.toggle("open", s === el && !o.typing));
    let rows = "";
    if (seg === "mod") {
      let rs = moduleRows();
      if (o.typing != null) {
        const t = o.typing.toLowerCase();
        const pre = rs.filter((r) => r.n.startsWith(t)), has = rs.filter((r) => !r.n.startsWith(t) && r.n.includes(t));
        rs = [...pre, ...has].map((r, k) => ({ ...r, sel: k === 0, html: esc(r.n).replace(t, `<b class="hmatch">${esc(t)}</b>`) }));
        if (!rs.length) rs = [{ more: true, n: `ask for “${o.typing}”` }];
      } else if (o.sel) rs = rs.map((r) => ({ ...r, sel: r.n === o.sel }));
      rows = rs.map((r) => row(r)).join("");
    } else if (seg === "item") {
      rows = itemRows().map((r) => row({ ...r, sel: r.n === o.sel })).join("");
    } else if (seg === "deeper") {
      const { vs, ms } = memberRows();
      rows = vs.map((r) => row(r)).join("") + `<div class="hmgap"></div>` + ms.slice(0, 5).map((r) => row({ ...r, sel: r.n === o.sel })).join("")
        + row({ more: true, n: `and ${ms.length - 5} more methods` });
    } else if (seg === "pkg") {
      const { yours, ext } = packageRows();
      rows = yours.map((r) => row(r)).join("") + `<div class="hmgap"></div>` + ext.map((r) => row(r)).join("");
    }
    menu(el, rows, { dx: -4 });
  }
  function openBack() {
    closeMenus();
    const b = q(".hback"); b.classList.add("on");
    menu(b, BACK.map((p) => row({ k: p.k, n: p.n, w: p.w })).join(""), { dx: 0, cls: "hbackm" });
  }
  function closeMenus() { layer.querySelectorAll(".hmenuw").forEach((m) => m.remove()); win.querySelectorAll(".hseg.open,.hnav.on").forEach((s) => s.classList.remove("open", "on")); }

  // ------------------------------------------------------------------ the hand at rest: marks in the foot
  const foot = q(".cstatus"); if (foot) foot.classList.add("hfoot");
  const reader = q("main") || q(".creader") || q(".reader");
  const handX = () => (reader ? rel(reader).x : 0) + (WIN().w <= 520 ? 14 : 20);
  function marks(keys, o = {}) {
    if (!foot) return null;
    foot.querySelectorAll(".hmarks").forEach((m) => m.remove());
    if (!keys.length && !o.landing) return null;
    const A = arrange(keys);
    const inner = A.order.map((x, k) => {
      const h = HELD[keyOf(x.i)], n = N[x.i];
      return `<span class="hm${!GENERIC && x.i === HERE ? " hnow" : ""}${h.hollow ? " hollow" : ""}${x.gap ? " apart" : ""}" data-i="${x.i}" title="${esc(n.n)}">${kmark(n.k)}</span>`;
    }).join("");
    const m = html(`<div class="hmarks" style="left:${handX() - rel(foot).x}px">${inner}<span class="hopen"></span></div>`);
    foot.appendChild(m);
    // the address yields to the hand: cut from the left, then gone
    const addr = foot.querySelector(".mono, span"); if (addr && addr !== m && !addr.closest(".hmarks")) {
      const full = addr.dataset.full || addr.textContent; addr.dataset.full = full;
      const room = rel(m).x - rel(addr).x - 18;
      if (room < 60) addr.style.visibility = "hidden";
      else { addr.style.visibility = ""; addr.textContent = full; if (rel(addr).w > room) { const name = full.split("/").pop(); addr.textContent = "…/" + name; if (rel(addr).w > room) addr.style.visibility = "hidden"; } }
    }
    return m;
  }

  // ------------------------------------------------------------------ the hand opened: cards and joins
  function joinHTML(j, o = {}) {
    if (!j) return "";
    const verb = j.how === "as" ? `<em>as</em>` : j.how === "steps" ? `<code>${esc(HC.verbs(j).join(" · "))}</code>` : "";
    return `<span class="hj${o.cls ? " " + o.cls : ""}"><i class="hln"></i>${verb}${verb ? `<i class="hln end"></i>` : `<i class="hln end" style="width:14px"></i>`}</span>`;
  }
  function cardHTML(i, o = {}) {
    const h = HELD[keyOf(i)] || {}, n = N[i];
    return `<button class="hc${i === HERE ? " hnow" : ""}${h.hollow ? " hollow" : ""}${o.hov ? " hov" : ""}" data-i="${i}">${kmark(n.k)}${esc(n.n)}<span class="x" title="let go"><svg viewBox="0 0 24 24"><path d="M6 6l12 12M18 6 6 18" stroke="currentColor" stroke-width="2" fill="none"></path></svg></span></button>`;
  }
  // Rust, tokenised first and escaped per token (so `<`, `&` and `;` never meet an entity)
  const hl = (src) => src.replace(/(\b(?:let|fn|pub|mut|enum|type|trait|struct|where|dyn|impl)\b)|(\b[A-Z]\w*\b)|(\b[a-z_]\w*(?=\s*[(<]))|(\?|;|::|->|[<>&(){},:=])|([^]*?(?=\b(?:let|fn|pub|mut|enum|type|trait|struct|where|dyn|impl)\b|\b[A-Z]|\b[a-z_]\w*\s*[(<]|\?|;|::|->|[<>&(){},:=]|$))/g,
    (m, kw, ty, ca, p, rest) => kw ? `<span class="kw">${esc(kw)}</span>` : ty ? `<span class="ty">${esc(ty)}</span>` : ca ? `<span class="ca">${esc(ca)}</span>` : p ? `<span class="p">${esc(p)}</span>` : esc(rest || ""));
  const doc1 = (n) => (n.d || "").split("\n")[0].replace(/\*\*|`/g, "").replace(/\[([^\]]+)\]\([^)]*\)/g, "$1");
  const sigOf = (n) => { const s = n.s || n.n; return n.v === "pub" && !/^pub\b/.test(s) ? "pub " + s : s; };
  function handHTML(keys, o = {}) {
    const A = arrange(keys);
    const rowHTML = A.order.map((x, k) => (x.gap ? `<span class="hgap"></span>` : "") + joinHTML(x.j) + cardHTML(x.i, { hov: o.hover === x.i, k })).join("");
    const say = A.main ? `<div class="hsay"><i>from</i> ${A.say}</div>` : "";
    const code = o.code && A.main ? `<pre class="hcode">${hl(A.code)}</pre>` : "";
    return { A, html: `<div class="hhandw"><div class="hhand"><div class="hrow">${rowHTML}</div>${say}${code}</div></div>` };
  }
  function openHand(keys, o = {}) {
    layer.querySelectorAll(".hhandw,.hpeekw").forEach((m) => m.remove());
    const { A, html: s } = handHTML(keys, o);
    const el = html(s); layer.appendChild(el);
    const wr = WIN(), fh = foot ? rel(foot).h : 26;
    el.style.left = handX() + "px"; el.style.bottom = (fh + 10) + "px";
    const room = wr.w - handX() - 14;
    if (rel(el).w > room) { el.querySelector(".hhand").classList.add("hcol"); el.style.right = "14px"; }
    if (o.hover != null) peekFor(o.hover, el);
    return { el, A };
  }
  function peekFor(i, handEl) {
    const card = handEl.querySelector(`.hc[data-i="${i}"]`); if (!card) return;
    const n = N[i], h = HELD[keyOf(i)], a = rel(card), hr = rel(handEl);
    const kindWord = { enum: "enum", type: "type", function: "function", trait: "trait", struct: "struct" }[n.k] || n.k;
    const sig = sigOf(n);
    const p = html(`<div class="hpeekw" style="left:${Math.round(a.x - 6)}px;bottom:${Math.round(WIN().h - hr.y + 8)}px"><div class="peek hpeek">
      <div class="ph">${kmark(n.k, "lg")}<div style="min-width:0"><div class="nm">${esc(n.n)}</div><div class="wh">${kindWord} in <code>${esc(where(i))}</code></div></div></div>
      <div class="psig">${hl(sig)}</div>
      <div class="say">${esc(doc1(n))}</div>
      <div class="foot">${esc(h.why)}, <b>${esc(h.ago)}</b></div></div></div>`);
    layer.appendChild(p);
  }

  // ------------------------------------------------------------------ motion: the same maths as facet::motion
  const bez = (x1, y1, x2, y2) => (x) => { // WebKit's solver, enough for stills
    const cx = 3 * x1, bx = 3 * (x2 - x1) - cx, ax = 1 - cx - bx, cy = 3 * y1, by = 3 * (y2 - y1) - cy, ay = 1 - cy - by;
    let t = x; for (let k = 0; k < 8; k++) { const xx = ((ax * t + bx) * t + cx) * t - x, dx = (3 * ax * t + 2 * bx) * t + cx; if (Math.abs(xx) < 1e-6 || !dx) break; t -= xx / dx; }
    return ((ay * t + by) * t + cy) * t;
  };
  const GLIDE = bez(.22, 1, .36, 1), DROP = bez(.5, 0, .9, .6);
  const clamp01 = (v) => Math.max(0, Math.min(1, v));
  const band = (t, a, b) => clamp01((t - a) / (b - a));
  // closed-form spring from 0 to 1: response s, damping ζ
  function spring(ms, response, zeta) {
    if (ms <= 0) return 0; const t = ms / 1000, w = 2 * Math.PI / response;
    if (zeta >= 1) return 1 - (1 + w * t) * Math.exp(-w * t);
    const wd = w * Math.sqrt(1 - zeta * zeta);
    return 1 - Math.exp(-zeta * w * t) * (Math.cos(wd * t) + (zeta * w / wd) * Math.sin(wd * t));
  }
  const SNAPPY = (ms) => spring(ms, .28, .86), BOUNCY = (ms) => spring(ms, .42, .6), REEL = (ms) => spring(ms, .24, .92);

  // Take → hand: the stone (text-free) arcs from where you held it into the foot, lands on BOUNCY,
  // and the name unrolls from the mark. Only the stone travels, so no two legible texts ever cross.
  function takeFrame(ms) {
    layer.querySelectorAll(".hfly").forEach((e) => e.remove());
    const hero = q("main svg.gem, main .gem, .chero svg");
    const land = ms >= 380;
    const m = marks(land ? ["value"] : [], { landing: true });
    if (!hero) return;
    const hr = rel(hero), from = { x: hr.x + hr.w / 2, y: hr.y + hr.h / 2 };
    const fr = rel(foot); const to = { x: handX() + 9, y: fr.y + fr.h / 2 };
    if (!land) {
      const p = clamp01(ms / 380);
      const x = from.x + (to.x - from.x) * GLIDE(p), y = from.y + (to.y - from.y) * DROP(p);
      const fall = DROP(p), shrink = band(fall, .15, 1); const size = hr.w + (14 - hr.w) * shrink, light = 1 - shrink;
      const lift = ms < 60 ? -8 * (ms / 60) : -8 * (1 - p);
      // the checker's own test (mut=1): the title flies with the stone, as MOTION's first hand demo had it
      const title = Q.get("mut") === "1" ? `<span style="position:absolute;left:${(size * 1.4).toFixed(1)}px;top:${(size * .1).toFixed(1)}px;font:700 ${Math.max(12, size * .66).toFixed(1)}px/1 var(--display);color:var(--ink0);white-space:nowrap">Value</span>` : "";
      const el = html(`<div class="hfly" style="left:${(x - size / 2).toFixed(1)}px;top:${(y - size / 2 + lift).toFixed(1)}px;width:${size}px;height:${size}px">${gem("enum", Math.round(size), Math.max(0, light))}${title}</div>`);
      layer.appendChild(el);
    } else if (m) {
      const mk = m.querySelector(".hm"); const off = -10 * (1 - BOUNCY(ms - 380));
      mk.style.transform = `translateY(${off.toFixed(2)}px)`;
      // the name unrolls from the mark (560–760), then rolls back into it after a while
      const u = band(ms, 560, 760), back = band(ms, 2800, 3000);
      const nm = html(`<span class="hname" style="width:0">Value<i>in hand</i></span>`); m.insertBefore(nm, m.querySelector(".hopen"));
      const full = nm.scrollWidth; nm.style.width = (full * GLIDE(u) * (1 - GLIDE(back))).toFixed(1) + "px";
    }
  }

  // Let go: the card sinks through the hand's floor (drop, clipped), its joins roll up, the room
  // closes on SNAPPY, the new join draws, and only the count word rolls.
  let dropCache = null;
  function dropFrame(ms) {
    const before = HANDS[5], after = before.filter((k) => k !== "value");
    const leaving = HELD.value.i;
    if (!dropCache) {
      const a = openHand(before); const pos = new Map();
      a.el.querySelectorAll(".hc").forEach((c) => pos.set(+c.dataset.i, rel(c)));
      const lc = a.el.querySelector(`.hc[data-i="${leaving}"]`);
      const js = [lc.previousElementSibling, lc.nextElementSibling].filter((e) => e && e.classList.contains("hj")).map((e) => ({ html: e.outerHTML, r: rel(e) }));
      dropCache = { pos, lhtml: lc.outerHTML, lpos: rel(lc), js, rowR: rel(a.el.querySelector(".hrow")), trayW: rel(a.el.querySelector(".hhand")).w };
    }
    const C = dropCache;
    const b = openHand(after);
    const tray = b.el.querySelector(".hhand"), trayW = rel(tray).w;
    // the room closes on SNAPPY from 160 ms: every card travels from where it was painted,
    // each join rides with the card it leads into, and the tray's edge follows the last card
    const MUT = Q.get("mut") === "1"; // the checker's own test: travel before the card has gone
    const settle = SNAPPY(ms - (MUT ? 0 : 160));
    tray.style.width = (C.trayW + (trayW - C.trayW) * settle).toFixed(1) + "px";
    const newJoin = b.el.querySelector(`.hc[data-i="${HELD.table.i}"]`).previousElementSibling;
    b.el.querySelectorAll(".hrow > *").forEach((el) => {
      const card = el.classList.contains("hc") ? el : el.classList.contains("hj") || el.classList.contains("hgap") ? el.nextElementSibling : null;
      if (!card || el === newJoin) return;
      const was = C.pos.get(+card.dataset.i), now = rel(card);
      if (was) el.style.transform = `translateX(${((was.x - now.x) * (1 - settle)).toFixed(2)}px)`;
    });
    // the new join: the line draws (360–520, periwinkle while drawing), then its word unrolls (440–560)
    if (newJoin && newJoin.classList.contains("hj")) {
      const draw = band(ms, 360, 520), word = band(ms, 440, 560);
      newJoin.style.clipPath = `inset(-4px ${(100 - 100 * draw).toFixed(1)}% -4px 0)`;
      if (draw > 0 && draw < 1) newJoin.classList.add("drawing");
      const em = newJoin.querySelector("em"); if (em) { em.style.display = "inline-block"; em.style.overflow = "hidden"; em.style.width = (GLIDE(word) * 20).toFixed(1) + "px"; em.style.padding = word ? "" : "0"; }
    }
    // the leaving card sinks through the row's floor (drop, 0–200), clipped; its joins roll up (0–160)
    const clip = html(`<div class="hclip" style="left:${C.rowR.x}px;top:${C.rowR.y}px;width:${C.rowR.w}px;height:${C.rowR.h}px"></div>`);
    b.el.parentNode.appendChild(clip);
    const sink = MUT ? 0 : DROP(band(ms, 0, 200)) * (C.lpos.h + 6);
    if (ms < 200) clip.insertAdjacentHTML("beforeend", C.lhtml.replace('class="hc', `style="position:absolute;margin:0;left:${C.lpos.x - C.rowR.x}px;top:${C.lpos.y - C.rowR.y + sink}px" class="hc`));
    const roll = GLIDE(band(ms, 0, 160));
    C.js.forEach((j, k) => { if (roll < 1) { const w = j.r.w * (1 - roll); const x = k === 0 ? j.r.x : j.r.x + j.r.w - w; clip.insertAdjacentHTML("beforeend", `<div style="position:absolute;overflow:hidden;left:${(x - C.rowR.x).toFixed(1)}px;top:${j.r.y - C.rowR.y}px;height:${j.r.h}px;width:${w.toFixed(1)}px;display:flex;align-items:center;justify-content:${k === 0 ? "flex-start" : "flex-end"}">${j.html}</div>`); } });
    // only the count word rolls, three → two, on REEL (300–540); the rest of the line holds still
    const say = b.el.querySelector(".hsay");
    if (say && ms < 700) {
      const p = clamp01(REEL(ms - 300));
      say.innerHTML = say.innerHTML.replace(/in (\w+) step/, (_, w2) => `in <span class="reel"><span>three</span><span>${w2}</span></span> step`);
      const reel = say.querySelector(".reel"), [a1, a2] = reel.children; const w1 = a1.offsetWidth, w2 = a2.offsetWidth;
      reel.style.width = (w1 + (w2 - w1) * p).toFixed(1) + "px";
      for (const x of reel.children) x.style.transform = `translateY(${(-18 * p).toFixed(2)}px)`;
    }
    return b;
  }

  // ------------------------------------------------------------------ the docked column (Vast): the hand absorbs pins
  function dock(keys) {
    const body = q(".cbody"); if (!body) return;
    const A = arrange(keys);
    const cards = A.order.map((x) => {
      const n = N[x.i], h = HELD[keyOf(x.i)];
      const j = x.j ? `<div class="hdj">${x.j.how === "as" ? "<em>as</em>" : x.j.how === "steps" ? `<code>${esc(HC.verbs(x.j).join(" · "))}</code>` : ""}</div>` : x.gap ? `<div class="hdgap"></div>` : "";
      return `${j}<div class="hdc${x.i === HERE ? " hnow" : ""}${h.hollow ? " hollow" : ""}"><div class="hh1">${kmark(n.k)}<b>${esc(n.n)}</b><small>${esc(pkName(n.p))}</small></div>`
        + `<span class="hsig">${hl(sigOf(n))}</span><div class="hsy">${esc(doc1(n))}</div></div>`;
    }).join("");
    body.appendChild(html(`<aside class="hdock"><div class="hsay"><i>from</i> ${A.say}</div>${cards}<pre class="hcode" style="margin-top:18px;font-size:11.5px">${hl(A.code)}</pre></aside>`));
  }

  // ------------------------------------------------------------------ Orbit: resume is the hand, one rung up
  function orbitResume() {
    const r = q(".resume"); if (!r) return;
    const A = arrange(HANDS[3]);
    const rc = A.order.map((x) => `<b>${kmark(N[x.i].k)}${esc(N[x.i].n)}</b>`).join(`<i>→</i>`);
    r.classList.add("hres");
    const news = r.querySelector(".new");
    r.innerHTML = `<span class="l1">Continue <span class="rc">${rc}</span></span><span class="l2"><i>from</i> ${A.say.replace(/, <i>in \w+ steps?<\/i>/, "")} · <i>you left at</i> <b>Table</b>, 2 h ago</span>`;
    if (news) r.appendChild(news);
  }

  // ------------------------------------------------------------------ Browse: the same hand, holding packages to compare
  function browseHand() {
    const old = q(".handwrap"); if (!old) return;
    const items = [...old.querySelectorAll(".hc")].map((c) => { const svg = c.querySelector("svg"); return { mark: svg ? `<span class="k sm eco">${svg.outerHTML}</span>` : "", name: [...c.childNodes].filter((x) => x.nodeType === 3).map((x) => x.textContent).join("").trim() }; });
    old.remove();
    const cards = items.map((it, k) => `${k ? `<span class="hgap" style="width:6px"></span>` : ""}<button class="hc pk">${it.mark}${esc(it.name)}</button>`).join("");
    const words = ["", "one", "two", "three", "four", "five"];
    const el = html(`<div class="hhandw"><div class="hhand"><div class="hrow">${cards}<button class="hgo">compare ${words[items.length]} ${chev()}</button></div></div></div>`);
    layer.appendChild(el);
    el.style.left = handX() + "px"; el.style.bottom = ((foot ? rel(foot).h : 26) + 10) + "px";
  }

  // ------------------------------------------------------------------ states
  const T = Q.has("t") ? +Q.get("t") : null;
  function apply() {
    closeMenus(); layer.innerHTML = ""; dropCache = null;
    Object.keys(JUMP).forEach((k) => delete JUMP[k]);
    if (STATE === "keys") { JUMP.focus = true; JUMP.typing = "t"; }
    if (STATE === "siblings") JUMP.open = "mod";
    calmTitlebar();
    const s = STATE;
    if (HOST === "orbit") { orbitResume(); return; }
    if (HOST === "browse") { browseHand(); return; }
    if (GENERIC) { if (reader && foot) marks(HANDS[3]); return; }
    const resting = { rest: 3, siblings: 3, members: 3, keys: 3, cmd: 3, back: 3, rest5: 5, seam: 5, hand1: 1, hand3: 3, hand5: 5, hover: 3, code: 3, drop: 5, first: 0, dock: 0 }[s] ?? 3;
    if (s === "dock") { dock(HANDS[5]); if (foot) foot.querySelectorAll(".hmarks").forEach((m) => m.remove()); return; }
    if (s === "first") { takeFrame(T ?? 200); return; }
    if (s.startsWith("hand") || s === "hover" || s === "code") {
      openHand(HANDS[s === "hand1" ? 1 : s === "hand5" ? 5 : 3], { hover: s === "hover" ? HELD.value.i : null, code: s === "code" });
      return; // said once: while the hand is open, its marks are the cards
    }
    if (s === "drop") { dropFrame(T ?? 90); return; }
    marks(HANDS[resting]);
    if (s === "seam" && foot) foot.insertAdjacentHTML("afterbegin", `<div class="hseam"><i class="done" style="--w:3"></i><i class="done" style="--w:2"></i><i class="now" style="--w:2"></i><i style="--w:3"></i></div>`);
    if (s === "siblings") openSeg("mod", { sel: "table" });
    if (s === "members") { q('.hseg[data-seg="deeper"]').classList.add("show"); openSeg("deeper", { sel: "as_table" }); }
    if (s === "keys") openSeg("mod", { typing: "t" });
    if (s === "back") openBack();
  }
  apply();
  if (cmdHeld) caps();

  // ------------------------------------------------------------------ live: the board answers the pointer and keys
  let live = { hand: resting0(), open: false };
  function resting0() { return HANDS[{ rest5: 5, seam: 5, hand1: 1, hand5: 5, drop: 5 }[STATE] ?? 3].slice(); }
  win.addEventListener("click", (e) => {
    const seg = e.target.closest(".hseg"); if (seg) { const was = seg.classList.contains("open"); if (was) closeMenus(); else openSeg(seg.dataset.seg); e.stopPropagation(); return; }
    const bk = e.target.closest(".hback"); if (bk) { if (bk.classList.contains("on")) closeMenus(); else openBack(); return; }
    const mk = e.target.closest(".hm, .hopen"); if (mk) { openHand(live.hand); return; }
    if (!e.target.closest(".hmenuw, .hhandw")) { closeMenus(); layer.querySelectorAll(".hhandw,.hpeekw").forEach((m) => m.remove()); if (HOST === "page2" && !["dock"].includes(STATE)) marks(live.hand); }
  });
  win.addEventListener("mouseover", (e) => {
    const c = e.target.closest(".hhandw .hc"); layer.querySelectorAll(".hpeekw").forEach((m) => m.remove());
    win.querySelectorAll(".hhandw .hc.hov").forEach((x) => x.classList.remove("hov"));
    if (c && c.dataset.i) { c.classList.add("hov"); peekFor(+c.dataset.i, c.closest(".hhandw")); }
  });
  d.addEventListener("keydown", (e) => {
    if (e.key === "Escape") { closeMenus(); layer.querySelectorAll(".hhandw,.hpeekw").forEach((m) => m.remove()); }
    if (e.metaKey && e.key === "l") { e.preventDefault(); q(".hjump").classList.add("focus"); openSeg("item"); }
    if (!e.metaKey && (e.key === "h" || e.key === "H") && HOST === "page2") openHand(live.hand);
    if (e.metaKey) { d.body.classList.add("cmd"); caps(); }
  });
  d.addEventListener("keyup", (e) => { if (!e.metaKey && !cmdHeld) d.body.classList.remove("cmd"); });

  // ------------------------------------------------------------------ films and the overlap law
  // Plays a motion state in real time, or samples it: ?check=1 reports every frame in which two
  // legible texts intersect (the law added to MOTION.md), on body[data-overlaps].
  const motion = { first: [takeFrame, 1000], drop: [dropFrame, 700] }[STATE];
  if (motion && PLAY) { const t0 = performance.now(); const tick = () => { const ms = performance.now() - t0; layer.innerHTML = ""; dropCache = null; if (STATE === "drop") dropCache = null; motion[0](Math.min(ms, motion[1])); if (ms < motion[1]) requestAnimationFrame(tick); }; requestAnimationFrame(tick); }
  if (motion && Q.get("check") === "1") {
    const bad = [];
    // a text's box is its own glyph runs (a Range over its text nodes), cut by every clipping ancestor
    const withText = (el) => [...el.childNodes].some((c) => c.nodeType === 3 && c.textContent.trim());
    let statics = null; // the page's own texts near where the hand moves (the page itself holds still)
    const near = () => {
      if (statics) return statics;
      const f = rel(q(".cstatus") || win), band = { y0: (reader ? rel(reader).y : 0) + 40, y1: f.b };
      statics = [...win.querySelectorAll("*")].filter((el) => !el.closest(".hlayer, .hmarks") && withText(el)).filter((el) => { const r = rel(el); return r.b > band.y0 - 400 && r.y < band.y1 + 4; });
      return statics;
    };
    const texts = () => [...layer.querySelectorAll("*"), ...win.querySelectorAll(".hmarks *"), ...near()].filter((el) => {
      if (!withText(el)) return false;
      const cs = fw.getComputedStyle(el); return cs.visibility !== "hidden" && cs.display !== "none" && +cs.opacity >= .35;
    }).map((el) => {
      const rg = d.createRange(); const ns = [...el.childNodes].filter((c) => c.nodeType === 3 && c.textContent.trim());
      rg.setStartBefore(ns[0]); rg.setEndAfter(ns[ns.length - 1]); const r = rg.getBoundingClientRect();
      let x0 = r.left, y0 = r.top, x1 = r.right, y1 = r.bottom;
      for (let p = el.parentElement; p && p !== win; p = p.parentElement) { const cs = fw.getComputedStyle(p); if (cs.overflow !== "visible" || cs.clipPath !== "none") { const pr = p.getBoundingClientRect(); x0 = Math.max(x0, pr.left); y0 = Math.max(y0, pr.top); x1 = Math.min(x1, pr.right); y1 = Math.min(y1, pr.bottom); } }
      return { el, x0, y0, x1, y1, moving: !!el.closest(".hlayer, .hmarks") };
    }).filter((b) => b.x1 - b.x0 > 1 && b.y1 - b.y0 > 1);
    // both texts are seen at the crossing only if nothing opaque is painted between them there
    const opaque = (el) => { const c = fw.getComputedStyle(el).backgroundColor.match(/[\d.]+/g); return c && (c.length < 4 || +c[3] > .9) && c.slice(0, 3).some((v) => +v > 0); };
    const bothSeen = (a, b, x, y) => {
      const stack = d.elementsFromPoint(x, y); const ia = stack.findIndex((e) => e === a.el || a.el.contains(e)), ib = stack.findIndex((e) => e === b.el || b.el.contains(e));
      if (ia < 0 || ib < 0) return false;
      for (let k = Math.min(ia, ib) + 1; k < Math.max(ia, ib); k++) if (opaque(stack[k])) return false;
      return true;
    };
    win.classList.add("hchecking");
    for (let ms = 0; ms <= motion[1]; ms += 20) {
      layer.innerHTML = ""; dropCache = null; if (STATE === "first") marks([]); motion[0](ms);
      const bs = texts(); const mv = bs.filter((b) => b.moving);
      for (const a of mv) for (const b of bs) {
        if (a === b || a.el.contains(b.el) || b.el.contains(a.el)) continue;
        const ix = Math.min(a.x1, b.x1) - Math.max(a.x0, b.x0), iy = Math.min(a.y1, b.y1) - Math.max(a.y0, b.y0);
        if (ix > 1 && iy > 1 && bothSeen(a, b, Math.max(a.x0, b.x0) + ix / 2, Math.max(a.y0, b.y0) + iy / 2)) bad.push(`${ms}ms: "${a.el.textContent.trim().slice(0, 20)}" × "${b.el.textContent.trim().slice(0, 20)}"`);
      }
    }
    win.classList.remove("hchecking");
    document.body.dataset.overlaps = bad.length ? [...new Set(bad)].slice(0, 12).join(" | ") : "none";
    layer.innerHTML = ""; dropCache = null; apply();
  }
  window.HAND_DEBUG = { frame: (ms) => { layer.innerHTML = ""; dropCache = null; if (motion) motion[0](ms); }, win, rel };
  document.body.dataset.ready = "1";
})();
