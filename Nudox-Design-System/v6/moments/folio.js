// The package page, legit-first. Top to bottom it answers what you ask in order when you meet a package:
//   1. What is it, and who made it            (the hero: name, what it says it is, by whom, where)
//   2. Should I trust it                       (five facts, each a component that opens on hover:
//                                               releases, licence, surface, weight, heads-up)
//   3. What does it say about itself           (its README, where every package it names is a preview)
//   4. What's in it — the words, all at once  (contents at a version: every public name, by module,
//                                               the way docs.rs lists all items; versions are the
//                                               same list seen at another release)
//   5. What it rests on, who uses it           (lists, progressively disclosed: everything beneath
//                                               unfolds one layer at a time)
import { gem } from "./paint.js";
import { comb, ago, weight, kindOf } from "./charts.js";
import { chip, card as licCard, verdict } from "./license.js";
import { heads, link, kLines, tint } from "./preview.js";
import { below, fmt, plural } from "./world.js";
import { CARRY, play, lerp, clamp } from "./motion.js";

const esc = (s) => String(s).replace(/[&<>"]/g, (c) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;" }[c]));
const fileOf = (id) => id.replace(/[/+]/g, "_");
const detailCache = new Map();
export async function detail(id) {
  if (detailCache.has(id)) return detailCache.get(id);
  let d = null;
  try { const r = await fetch(`data/pkg/${fileOf(id)}.json`, { cache: "reload" }); if (r.ok) d = await r.json(); } catch { d = null; }
  detailCache.set(id, d);
  return d;
}

// README text → HTML: `code` spans, and every word that names a package in your library becomes a preview.
function prose(text, WD, self) {
  if (!text) return "";
  const names = new Map();
  for (const p of WD.all) if (p !== self && p.name.length > 2 && !names.has(p.name)) names.set(p.name, p);
  return text.split(/\n{2,}/).slice(0, 3).map((para) => {
    const parts = para.split(/(`[^`]+`)/g).map((seg) => {
      if (seg.startsWith("`") && seg.endsWith("`")) {
        const inner = seg.slice(1, -1), p = names.get(inner.replace(/::.*$/, ""));
        return p ? `<code>${link(p, inner)}</code>` : `<code>${esc(inner)}</code>`;
      }
      return esc(seg).replace(/\b([A-Za-z][\w-]{2,})\b/g, (w) => { const p = names.get(w); return p && /[_-]|^[a-z]/.test(w) && w.length > 3 ? link(p, w) : w; });
    });
    return `<p>${parts.join("")}</p>`;
  }).join("");
}

export function folioView(host, WD, { TRUST, onHop, onGraph }) {
  let P = null, T = null, D = null, at = null, preview = null;

  // ---------------------------------------------------------------- 1. hero
  function hero(p) {
    const by = T && T.authors && T.authors.length ? `by ${T.authors.slice(0, 2).map(esc).join(", ")}${T.authors.length > 2 ? ` and ${T.authors.length - 2} more` : ""}` : "";
    const repo = T && T.repository ? `<span class="url">${esc(T.repository.replace(/^https?:\/\/(www\.)?/, "").replace(/\.git$/, ""))}</span>` : "";
    const ed = T && (T.edition || T.rust_version) ? `<span>${T.edition ? `Rust ${esc(T.edition)}` : ""}${T.rust_version ? ` · needs ${esc(T.rust_version)}` : ""}</span>` : "";
    const cats = T && T.categories && T.categories.length ? `<span>${T.categories.slice(0, 2).map((c) => esc(c.replace(/::/g, " › "))).join(", ")}</span>` : "";
    return `<div class="f-hero"><span class="hg" data-g="${esc(p.id)}">${gem(56, tint(p))}</span><div class="ht">
      <div class="f-title"><h1 data-n="${esc(p.id)}">${esc(p.name)}</h1><span class="f-ver" data-ver>${esc(at || p.version)}<i>${at && at !== p.version ? "reading" : p.depth === 1 ? "your pin" : p.kind === "yours" ? "yours" : "in your tree"}</i></span></div>
      <div class="lede">${esc(p.lede || "")}</div>
      <div class="f-by">${[by, repo, cats, ed].filter(Boolean).join(`<i>·</i>`)}</div></div></div>`;
  }

  // ---------------------------------------------------------------- 2. the five facts
  function facts(p) {
    const rel = T && T.releases ? T.releases : [];
    const pinRel = rel.find((r) => r.v === p.version);
    const behind = p.newer ? p.newer.length : 0;
    const v = verdict(p.license);
    const fam = { type: 0, callable: 0, contract: 0, value: 0 };
    const mods = D ? D.modules : p.modules;
    for (const m of mods) for (const it of m.items) fam[it.f] = (fam[it.f] || 0) + 1;
    const total = Object.values(fam).reduce((a, b) => a + b, 0) || 1;
    const famBar = `<div class="fam">${Object.entries(fam).map(([k, n]) => `<i class="${k}" style="flex:${n}"></i>`).join("")}</div>`;
    const beneath = below(p);
    const sl = (q) => (TRUST[q.id] && TRUST[q.id].sloc) || 0;
    const direct = p.depsP.reduce((s, q) => s + sl(q), 0), deep = [...beneath].reduce((s, q) => s + sl(q), 0);
    const own = T && T.sloc ? T.sloc : 0;
    const huN = T ? [T.build_rs, T.proc_macro, ...(["net", "fs", "process", "env", "ffi"].map((k) => (T.caps || {})[k + "_n"]))].filter(Boolean).length : 0;
    return `<div class="f-facts">
      <div class="fact" data-fact="releases"><div class="fk">Releases</div><div class="fv">${fmt(T ? T.n || rel.length : 0)}<span>since ${T && T.first ? T.first.slice(0, 4) : "?"}</span></div>
        ${comb(rel, { w: 186, h: 24, pin: p.version, latest: p.latest })}
        <div class="fs">newest <b class="am">${esc(p.latest || p.version)}</b> ${ago(T && T.last)}</div>
        <div class="fs">${pinRel ? `your pin ${ago(pinRel.t).replace(" ago", " old")}` : ""}${behind ? ` · <span class="am">${behind} behind</span>` : " · up to date"}</div></div>
      <div class="fact" data-fact="licence"><div class="fk">Licence</div><div class="fv sm">${chip(p.license)}</div>
        <div class="fs"><b class="${v.tone}">${v.word}</b></div><div class="fs clamp">${esc(v.line)}</div></div>
      <div class="fact" data-fact="surface"><div class="fk">Surface</div><div class="fv">${fmt(total)}<span>public names</span></div>${famBar}
        <div class="fs">${plural(mods.length, "module")}${own ? ` · ${kLines(own)} lines` : ""}</div>
        <div class="fs">${p.sites ? `<b class="y">you use ${fmt(p.used.size)}</b> · ${plural(p.sites, "place")}` : T && T.tests ? "has tests" : ""}${T && T.examples ? ` · ${plural(T.examples, "example")}` : ""}</div></div>
      <div class="fact" data-fact="weight"><div class="fk">Weight</div><div class="fv">${fmt(beneath.size)}<span>packages beneath</span></div>
        ${weight([{ n: own, cls: "own" }, { n: direct, cls: "direct" }, { n: Math.max(0, deep - direct), cls: "deep" }], 186)}
        <div class="fs">${plural(p.depsP.length, "direct")} · ${kLines(deep)} lines beneath</div>
        <div class="fs">${beneath.size ? `its own code is ${Math.round((own / Math.max(1, own + deep)) * 100)}% of it` : "stands alone"}</div></div>
      <div class="fact" data-fact="heads"><div class="fk">Heads-up</div><div class="fv sm">${huN ? `${huN}<span>things it does</span>` : `<span class="ok">nothing notable</span>`}</div>
        <div class="f-heads">${heads(T)}</div>
        <div class="fs quiet">Advisories: no feed configured</div></div>
    </div>`;
  }

  // ---------------------------------------------------------------- 4. contents at a version
  function contents(p) {
    const rel = T && T.releases ? T.releases : [];
    const local = new Set(T && T.local_versions ? T.local_versions : [p.version]);
    // The crate's front door first (its root), then the rest by size, as docs.rs leads with the crate root.
    const mods = (D ? D.modules : p.modules).filter((m) => m.items.length).slice().sort((a, b) => (b.path === "lib") - (a.path === "lib") || b.items.length - a.items.length);
    const shown = mods.filter((m) => !m.private);
    const hidden = mods.filter((m) => m.private);
    const word = (it, st = "") => `<span class="w ${it.f} ${p.used.has(it.n) ? "y" : ""} ${st}" data-it="${esc(it.n)}"><i></i><span class="t">${esc(it.n)}</span></span>`;
    const row = (m, cls = "") => `<div class="mrow ${cls}"><div class="mp">${esc(m.path)}<span>${m.items.length}</span></div><div class="ws">${m.items.map((it) => word(it, it._st || "")).join("")}</div></div>`;
    return `<div class="f-sec f-contents"><div class="f-sh"><h2>Contents</h2><span class="at">at <b data-at>${esc(at || p.version)}</b> <i>${local.size > 1 ? `${local.size} releases read · hover the comb` : "the release your tree has"}</i></span>
        <span class="f-toggle"><b class="on">words</b><b>map</b></span></div>
      <div class="f-comb">${comb(rel, { w: 1096, h: 30, pin: p.version, latest: p.latest, labels: true })}<div class="f-local">${[...local].map((v) => `<span data-v="${esc(v)}" class="${v === (at || p.version) ? "on" : ""}">${esc(v)}</span>`).join("")}</div></div>
      <div class="f-diff"></div>
      <div class="mrows">${shown.map((m) => row(m)).join("")}${hidden.length ? `<div class="mhid">${plural(hidden.reduce((s, m) => s + m.items.length, 0), "name")} in ${plural(hidden.length, "private module")}, reachable only if re-exported <u>show</u></div><div class="mrows-hid">${hidden.map((m) => row(m, "priv")).join("")}</div>` : ""}</div></div>`;
  }

  // ---------------------------------------------------------------- 5. rests on / used by, progressively
  function relations(p) {
    const T2 = (q) => TRUST[q.id] || {};
    const depRow = (q, depth = 0) => {
      const t = T2(q);
      const kids = q.depsP.length;
      return `<div class="drow" style="padding-left:${depth * 18}px" data-id="${esc(q.id)}">
        ${kids ? `<b class="tw" data-open="${esc(q.id)}"></b>` : `<b class="tw none"></b>`}${gem(13, tint(q))}${link(q)}<span class="dv">${esc(q.version)}</span>
        <span class="dh">${t.proc_macro ? `<span class="hu amber">proc-macro</span>` : ""}${t.build_rs ? `<span class="hu amber">build.rs</span>` : ""}</span>
        <span class="dn">${kids ? plural(kids, "dep") : ""}</span><span class="dl">${t.sloc ? kLines(t.sloc) : ""}</span></div><div class="dkids" data-kids="${esc(q.id)}"></div>`;
    };
    const deps = p.depsP.slice().sort((a, b) => (TRUST[b.id]?.sloc || 0) - (TRUST[a.id]?.sloc || 0));
    const mine = p.dependentsP.filter((d) => d.kind === "yours").sort((a, b) => ((p.uses?.by || {})[b.name] || 0) - ((p.uses?.by || {})[a.name] || 0));
    const others = p.dependentsP.filter((d) => d.kind !== "yours");
    const all = below(p).size;
    return `<div class="f-sec f-rel"><div class="f-col"><div class="f-sh"><h2>Rests on</h2><span class="at">${plural(p.depsP.length, "package")} · ${fmt(all)} with everything beneath</span></div>
        ${deps.map((q) => depRow(q)).join("") || `<div class="none">Rests on nothing. It stands alone.</div>`}</div>
      <div class="f-col"><div class="f-sh"><h2>Used by</h2><span class="at">${plural(p.dependentsP.length, "package")} in your library</span><span class="f-graph" data-graph>as a graph</span></div>
        ${mine.map((d) => `<div class="urow">${gem(13, tint(d))}${link(d, d.name.replace(/^backend-/, ""), "y")}<span class="un y">${(p.uses?.by || {})[d.name] ? plural(p.uses.by[d.name], "place") : ""}</span></div>`).join("")}
        ${others.slice(0, 8).map((d) => `<div class="urow">${gem(13, tint(d))}${link(d)}<span class="un">${esc(d.version)}</span></div>`).join("")}
        ${others.length > 8 ? `<div class="urow more">+ ${fmt(others.length - 8)} more</div>` : ""}</div></div>`;
  }

  // ---------------------------------------------------------------- the page
  async function render(p, { fromP = null } = {}) {
    P = p; T = TRUST[p.id] || null; at = null;
    D = await detail(p.id);
    host.innerHTML = `<div class="folio2">${hero(p)}${facts(p)}
      ${T && T.readme ? `<div class="f-sec f-about"><div class="f-sh"><h2>About</h2><span class="at">from its README</span></div><div class="prose">${prose(T.readme, WD, p)}</div></div>` : ""}
      ${contents(p)}${relations(p)}</div>`;
    wire(p);
    if (fromP) { const el = host.querySelector(`[data-pkg="${CSS.escape(fromP.id)}"]`); if (el) el.closest(".drow,.urow")?.classList.add("from"); }
  }
  function wire(p) {
    // Unfold beneath one layer at a time.
    host.querySelectorAll("[data-open]").forEach((b) => (b.onclick = (e) => {
      e.stopPropagation();
      const q = WD.byId.get(b.dataset.open), kids = host.querySelector(`[data-kids="${CSS.escape(q.id)}"]`);
      if (kids.innerHTML) { kids.innerHTML = ""; b.classList.remove("open"); return; }
      const depth = (parseInt(b.closest(".drow").style.paddingLeft) || 0) / 18 + 1;
      kids.innerHTML = q.depsP.map((k) => relationsRow(k, depth)).join("");
      b.classList.add("open");
      kids.querySelectorAll(".drow").forEach((r, i) => { r.style.opacity = 0; r.style.transform = "translateY(-4px)"; setTimeout(() => { r.style.transition = "opacity 160ms, transform 200ms"; r.style.opacity = 1; r.style.transform = "none"; }, 18 * i); });
      wire(p);
    }));
    const hid = host.querySelector(".mhid u"); if (hid) hid.onclick = () => host.querySelector(".mrows-hid").classList.toggle("on");
    host.querySelectorAll(".f-local [data-v]").forEach((s) => { s.onmouseenter = () => showAt(s.dataset.v); s.onclick = () => { at = s.dataset.v === P.version ? null : s.dataset.v; showAt(s.dataset.v, true); }; });
    host.querySelector(".f-local")?.addEventListener("mouseleave", () => showAt(at || P.version));
    const g = host.querySelector("[data-graph]"); if (g && onGraph) g.onclick = () => onGraph(P);
    // words ⇄ map: the same list; in the map every word folds into its mark, and the marks close up into shingles.
    host.querySelectorAll(".f-toggle b").forEach((b, i) => (b.onclick = () => {
      host.querySelectorAll(".f-toggle b").forEach((x, j) => x.classList.toggle("on", i === j));
      host.querySelector(".f-contents").classList.toggle("map", i === 1);
    }));
    host.querySelectorAll(".fact").forEach((f) => { f.onmouseenter = () => factCard(f); f.onmouseleave = () => factCard(null); });
    host.querySelectorAll(".w").forEach((w) => { w.onmouseenter = () => wordPeek(w); w.onmouseleave = () => wordPeek(null); });
  }
  function relationsRow(q, depth) {
    const t = TRUST[q.id] || {};
    return `<div class="drow" style="padding-left:${depth * 18}px" data-id="${esc(q.id)}">${q.depsP.length ? `<b class="tw" data-open="${esc(q.id)}"></b>` : `<b class="tw none"></b>`}${gem(13, tint(q))}${link(q)}<span class="dv">${esc(q.version)}</span>
      <span class="dh">${t.proc_macro ? `<span class="hu amber">proc-macro</span>` : ""}${t.build_rs ? `<span class="hu amber">build.rs</span>` : ""}</span><span class="dn">${q.depsP.length ? plural(q.depsP.length, "dep") : ""}</span><span class="dl">${t.sloc ? kLines(t.sloc) : ""}</span></div><div class="dkids" data-kids="${esc(q.id)}"></div>`;
  }

  // Contents at another release: the same list, recoloured (new, changed, gone), words never move.
  async function showAt(v, commit = false) {
    const box = host.querySelector(".f-contents .mrows"); if (!box) return;
    const diff = host.querySelector(".f-diff");
    host.querySelectorAll(".f-local [data-v]").forEach((s) => s.classList.toggle("on", s.dataset.v === v));
    host.querySelector("[data-at]").textContent = v;
    if (v === P.version) { box.querySelectorAll(".w").forEach((w) => w.classList.remove("new", "chg", "gone")); box.querySelectorAll(".w.ghost").forEach((w) => w.remove()); diff.innerHTML = ""; return; }
    const other = await detail(`${P.name}@${v}`);
    if (!other || !D) { diff.innerHTML = `<span class="quiet">${esc(v)} isn't read yet</span>`; return; }
    const sig = (d) => { const m = new Map(); for (const mod of d.modules) for (const it of mod.items) m.set(mod.path + "::" + it.n, { it, mod: mod.path }); return m; };
    const a = sig(D), b = sig(other);
    let added = 0, gone = 0, chg = 0, touched = 0;
    box.querySelectorAll(".w.ghost").forEach((w) => w.remove());
    box.querySelectorAll(".mrow").forEach((row) => {
      const mod = row.querySelector(".mp").firstChild.textContent;
      row.querySelectorAll(".w").forEach((w) => {
        const key = mod + "::" + w.dataset.it, x = a.get(key), y = b.get(key);
        w.classList.remove("new", "chg", "gone");
        if (!y) { w.classList.add("gone"); gone++; if (w.classList.contains("y")) touched++; }
        else if (x && y && x.it.s !== y.it.s) { w.classList.add("chg"); chg++; if (w.classList.contains("y")) touched++; }
      });
      const ws = row.querySelector(".ws");
      for (const [key, y] of b) if (y.mod === mod && !a.has(key)) { const s = document.createElement("span"); s.className = `w ${y.it.f} new ghost`; s.innerHTML = `<i></i><span class="t">${esc(y.it.n)}</span>`; ws.appendChild(s); added++; }
    });
    diff.innerHTML = `<b>${esc(P.version)} → ${esc(v)}</b><span class="new">${fmt(added)} new</span><span class="chg">${fmt(chg)} changed</span><span class="gone">${fmt(gone)} gone</span>${touched ? `<span class="hit">${plural(touched, "name")} you use change</span>` : P.sites ? `<span class="ok">nothing you use changes</span>` : ""}`;
    if (commit) host.querySelector(".f-ver").innerHTML = `${esc(v)}<i>reading</i>`;
  }

  // Hover cards for the facts and the words.
  const tip = document.createElement("div"); tip.className = "ftip"; document.body.appendChild(tip);
  function factCard(f) {
    if (!f) { tip.classList.remove("on"); return; }
    const k = f.dataset.fact; let html = "";
    if (k === "licence") html = licCard(P.license);
    else if (k === "releases" && T) {
      const rel = T.releases.slice().reverse();
      let prev = null; const kinds = new Map(); for (const r of T.releases) { kinds.set(r.v, kindOf(prev, r.v)); if (!r.y) prev = r.v; }
      html = `<div class="tip-h">Releases, newest first</div>${rel.slice(0, 12).map((r) => `<div class="rl ${r.y ? "yank" : ""} ${r.v === P.version ? "pin" : ""}"><b>${esc(r.v)}</b><span class="rk ${kinds.get(r.v)}">${kinds.get(r.v)}</span><span>${r.t || ""}</span><span class="ra">${r.y ? "yanked" : r.v === P.version ? "your pin" : ago(r.t)}</span></div>`).join("")}${rel.length > 12 ? `<div class="rl more">${fmt(rel.length - 12)} older</div>` : ""}`;
    } else if (k === "heads" && T) {
      const caps = T.caps || {};
      const ex = (key, word) => (caps[key] || []).length ? `<div class="hx"><div class="hx-h">${word} <b>${fmt(caps[key + "_n"] || 0)}</b></div>${caps[key].map(([f2, l, t]) => `<div class="hx-l"><span>${esc(f2)}:${l}</span><code>${esc(t)}</code></div>`).join("")}</div>` : "";
      html = `<div class="tip-h">What it does, from its own source</div>${T.build_rs ? `<div class="hx"><div class="hx-h">Runs a build script <b>build.rs</b></div><div class="hx-l">code that runs on your machine when you compile</div></div>` : ""}${T.proc_macro ? `<div class="hx"><div class="hx-h">Is a procedural macro</div><div class="hx-l">code that runs inside your compiler</div></div>` : ""}${ex("process", "Starts programs")}${ex("net", "Opens network connections")}${ex("fs", "Touches files")}${ex("env", "Reads environment variables")}${ex("ffi", "Calls C")}<div class="hx"><div class="hx-h">Unsafe <b>${T.forbid_unsafe ? "forbidden" : fmt(T.unsafe || 0)}</b></div></div>`;
    } else if (k === "weight") {
      const deps = P.depsP.map((q) => ({ q, n: below(q).size + 1, s: (TRUST[q.id]?.sloc || 0) + [...below(q)].reduce((s2, x) => s2 + (TRUST[x.id]?.sloc || 0), 0) })).sort((a, b) => b.s - a.s);
      const max = Math.max(1, ...deps.map((d) => d.s));
      html = `<div class="tip-h">Where the weight comes from</div>${deps.slice(0, 10).map((d) => `<div class="wl"><b>${esc(d.q.name)}</b><span class="wb"><i style="width:${(d.s / max) * 100}%"></i></span><span>${kLines(d.s)} · ${fmt(d.n)}</span></div>`).join("")}`;
    } else return;
    tip.innerHTML = html; tip.classList.add("on");
    const r = f.getBoundingClientRect(); const tw = tip.getBoundingClientRect().width || 400; tip.style.left = Math.max(8, Math.min(innerWidth - tw - 12, r.right - tw)) + "px"; tip.style.top = r.bottom + 8 + "px";
  }
  function wordPeek(w) {
    if (!w) { tip.classList.remove("on"); return; }
    const mod = w.closest(".mrow").querySelector(".mp").firstChild.textContent;
    const src = D ? D.modules.find((m) => m.path === mod) : null;
    const it = src ? src.items.find((x) => x.n === w.dataset.it) : null;
    tip.innerHTML = `<div class="wp"><div class="wp-t"><span class="km2 ${it ? it.f : "value"}"></span><b>${esc(w.dataset.it)}</b><span>${esc(mod)}</span></div>${it && it.s ? `<code>${esc(it.s)}</code>` : ""}${it && it.d ? `<div class="wp-d">${esc(it.d)}</div>` : ""}${w.classList.contains("y") ? `<div class="wp-y">your code names it ${plural(P.used.get(w.dataset.it) || 0, "time")}</div>` : ""}</div>`;
    tip.classList.add("on");
    const r = w.getBoundingClientRect(); tip.style.left = Math.min(innerWidth - 420, r.left) + "px"; tip.style.top = r.bottom + 6 + "px";
  }

  // ---------------------------------------------------------------- hop: the link you followed becomes the title
  async function hop(Q, anchor) {
    const old = P;
    const a = anchor.getBoundingClientRect();
    const oldTitle = host.querySelector(".f-hero h1").getBoundingClientRect(), oldGem = host.querySelector(".f-hero .hg").getBoundingClientRect();
    const fly = document.createElement("div"); fly.className = "ghosts";
    fly.innerHTML = `<div class="fly nm" data-k="new">${esc(Q.name)}</div><div class="fly" data-k="newg">${gem(56, tint(Q))}</div><div class="fly nm" data-k="old">${esc(old.name)}</div>`;
    document.body.appendChild(fly);
    const [nNew, gNew, nOld] = fly.children;
    const aSize = parseFloat(getComputedStyle(anchor).fontSize);
    host.style.transition = "opacity 120ms"; host.style.opacity = "0";
    await new Promise((r) => setTimeout(r, 120));
    await render(Q, { fromP: old });
    host.scrollTop = 0; host.parentElement && (host.parentElement.scrollTop = 0);
    const title = host.querySelector(".f-hero h1"), g = host.querySelector(".f-hero .hg");
    const to = title.getBoundingClientRect(), tg = g.getBoundingClientRect();
    const back = host.querySelector(`[data-pkg="${CSS.escape(old.id)}"]`);
    const tb = back ? back.getBoundingClientRect() : null;
    title.style.visibility = "hidden"; g.style.visibility = "hidden"; if (back) back.style.visibility = "hidden";
    const rest = [...host.querySelectorAll(".f-hero .lede, .f-hero .f-by, .f-facts .fact, .f-sec")];
    rest.forEach((el) => (el.style.opacity = 0));
    host.style.opacity = "1";
    await play(640, (ms) => {
      const u = CARRY(ms);
      const size = lerp(aSize, 40, u);
      Object.assign(nNew.style, { left: lerp(a.left, to.left, u) + "px", top: lerp(a.top, to.top, u) + "px", font: `700 ${size}px/1.1 "Bricolage Grotesque"`, letterSpacing: `${-0.03 * size}px`, color: "var(--ink0)" });
      const gs = lerp(14, 56, u);
      Object.assign(gNew.style, { left: lerp(a.left - 20, tg.left, u) + "px", top: lerp(a.top, tg.top, u) + "px", width: gs + "px", height: gs + "px" });
      if (tb) { const s2 = lerp(40, 13, u); Object.assign(nOld.style, { left: lerp(oldTitle.left, tb.left, u) + "px", top: lerp(oldTitle.top, tb.top, u) + "px", font: `${u > 0.96 ? 400 : 700} ${s2}px/1.2 ${u > 0.96 ? '"Geist Mono"' : '"Bricolage Grotesque"'}`, color: "var(--ink1)" }); }
      else nOld.style.opacity = 1 - clamp(ms / 200);
      rest.forEach((el) => { const r = el.getBoundingClientRect(); el.style.opacity = clamp((ms - 180 - (r.top - to.top) * 0.35) / 200); });
    });
    fly.remove(); title.style.visibility = ""; g.style.visibility = ""; if (back) back.style.visibility = "";
    rest.forEach((el) => (el.style.opacity = ""));
  }
  return { render, hop, get P() { return P; }, showAt };
}
