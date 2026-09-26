// The version comb in the shelf header, and the upgrade lens on the page, over window.RELEASES (releases.mjs).
//
// The comb: one tick per release. Tall = a breaking bump (a major, or a minor below 1.0), mid = a feature
// release, short = a patch. Mint = the version your lockfile pins, periwinkle = the release you are viewing.
// Hover names a release and its age; click or ←/→ scrubs; Esc comes home. Past ~1 release per 3 px the comb
// is a band and the pointer opens a fisheye so every release stays reachable.
//
// The lens: scrubbed away from your pin, the page says what changed between the two, for this symbol and for
// the places your code uses the crate: "Upgrading to 1.1.6 touches 2 of your 9 uses".
(() => {
  "use strict";
  let G = null;
  const esc = (s) => String(s).replace(/[&<>"]/g, (c) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;" })[c]);
  const R = () => window.RELEASES || {};
  const state = new Map(); // crate -> viewed version (absent = your pin)
  const sv = (v) => { const m = /^(\d+)\.(\d+)\.(\d+)/.exec(v || ""); return m ? [+m[1], +m[2], +m[3]] : [0, 0, 0]; };
  const cmp = (a, b) => { const x = sv(a), y = sv(b); return x[0] - y[0] || x[1] - y[1] || x[2] - y[2]; };
  function bump(prev, v) {
    if (!prev) return 2; const a = sv(prev), b = sv(v);
    if (b[0] !== a[0]) return 2; if (b[0] === 0 && b[1] !== a[1]) return 2; if (b[1] !== a[1]) return 1; return 0;
  }
  function ago(at) {
    if (!at) return ""; const d = (Date.now() - Date.parse(at)) / 864e5;
    if (d < 1) return "today"; if (d < 45) return `${Math.round(d)} days ago`; if (d < 540) return `${Math.round(d / 30)} months ago`; return `${(d / 365).toFixed(d < 3650 ? 1 : 0).replace(/\.0$/, "")} years ago`;
  }
  const crateOf = (i) => { const pk = G.PK[G.N[i].p]; return pk && R()[pk.name] ? pk.name : null; };
  const rel = (c) => R()[c];
  const viewed = (c) => state.get(c) || rel(c).pinned;
  const list = (c) => rel(c).versions.filter((x) => !x.yanked || x.v === rel(c).pinned);

  // ------------------------------------------------------------------ the comb
  const W = 236;
  function positions(n, fish) {
    const step = W / Math.max(1, n - 1); const xs = new Array(n);
    for (let k = 0; k < n; k++) {
      let x = k * step;
      if (fish) { const d = x - fish.c; if (Math.abs(d) <= fish.r) { const t = d / fish.r; x = fish.c + fish.r * Math.sign(t) * (1 - Math.pow(1 - Math.abs(t), fish.m)); } }
      xs[k] = x;
    }
    return xs;
  }
  function comb(c) {
    const vs = list(c); const pin = rel(c).pinned, view = viewed(c);
    const xs = positions(vs.length, null);
    const ticks = vs.map((x, k) => {
      const b = bump(k ? vs[k - 1].v : null, x.v); let h = [6, 12, 18][b], cls = "t";
      if (x.v === pin) { cls += " pin"; h = 20; } if (x.v === view && view !== pin) { cls += " view"; h = 22; }
      if (!x.local) cls += " far";
      return `<i class="${cls}" data-k="${k}" style="left:${xs[k].toFixed(2)}px;height:${h}px"></i>`;
    }).join("");
    return `<div class="vcomb" data-crate="${esc(c)}" tabindex="0" role="slider" aria-label="${esc(c)} release" aria-valuetext="${esc(view)}" style="width:${W}px">${ticks}<span class="ctip"></span></div>`;
  }
  function header(i) {
    const c = crateOf(i); if (!c) return "";
    const pin = rel(c).pinned, view = viewed(c);
    const line = view !== pin ? `<div class="vline">viewing <b>${esc(short(view))}</b><i>·</i>you pin <span>${esc(short(pin))}</span><kbd>esc</kbd></div>` + summary(c, pin, view) : "";
    return comb(c) + line;
  }
  // the crate-wide one-liner under the comb
  function summary(c, a, b) {
    const d = diffOf(c, a, b); if (!d) return `<div class="vsum">${esc(short(b))} is not on this machine; only its date is known</div>`;
    const anchor = anchorOf(c);
    const all = dedupe(changes(d), c, d); const moved = all.filter((x) => spellingOnly(x, anchor)).length;
    const br = all.filter((x) => x.kind === "breaking" && !spellingOnly(x, anchor)).length, ad = all.filter((x) => x.kind === "additive").length; const imp = impactOf(c, a, b); const real = imp.filter((u) => !spellingOnly(u.change, anchor)); const uses = (rel(c).uses || []).length;
    const you = !uses ? "" : real.length ? `<b class="y">${real.length}</b> of your ${uses} use${uses === 1 ? "" : "s"} change` : `none of your ${uses} use${uses === 1 ? "" : "s"} change`;
    return `<div class="vsum">${br ? `<b>${br}</b> breaking` : "nothing breaking"}<i>·</i>${ad} added${moved ? `<i>·</i>${moved} respelled` : ""}${you ? `<i>·</i>${you}` : ""}`
      + (d.semverSlip ? `<div class="vslip">breaking in a ${sv(a)[0] === sv(b)[0] && sv(a)[1] === sv(b)[1] ? "patch" : "minor"} release</div>` : "") + `</div>`;
  }

  // ------------------------------------------------------------------ diff access, tolerant of shape
  const key = (a, b) => `${a}→${b}`;
  function diffOf(c, a, b) { const D = rel(c).diff || {}; return D[key(a, b)] || D[`${a}->${b}`] || null; }
  function impactOf(c, a, b) { const I = rel(c).impact || {}; return I[key(a, b)] || I[`${a}->${b}`] || []; }
  const anchors = new Map(); // a node of the crate, so names in its signatures resolve inside it
  const anchorOf = (c) => { if (!anchors.has(c)) anchors.set(c, G.N.findIndex((n) => G.PK[n.p] && G.PK[n.p].name === c)); return anchors.get(c); };
  const short = (v) => String(v).replace(/\+.*$/, ""); // 1.1.6+spec-1.1.0 reads 1.1.6
  const what = (ch) => (ch && (ch.what || ch.type)) || "changed";
  // a change whose two signatures read the same in plain words is a spelling move, not a change to your code
  // Rust has no named arguments: a parameter's name and binding (`v`, `_v`, `mut v`) are the callee's
  // business, never the caller's. Signatures compare by their types, position by position.
  const bindings = (sig) => String(sig || "").replace(/([(,]\s*)(?:mut\s+)?\w+\s*:(?!:)/g, "$1");
  function spellingOnly(ch, from) {
    if (!ch || what(ch) !== "changed") return false;
    const pb = parts(bindings(ch.before), from), pa = parts(bindings(ch.after), from); if (!pb || !pa) return false;
    return text(side(pb, pa, "x")) === text(side(pa, pb, "x"));
  }
  const pathOf = (x) => (typeof x === "string" ? x : x.path || x.id || "");
  function changes(d) { // flatten to [{path, what, kind, before, after}]
    const out = [];
    for (const x of d.added || []) out.push({ path: pathOf(x), what: "added", kind: x.kind || "additive", after: x.sig || x.after, k: x.k });
    for (const x of d.removed || []) out.push({ path: pathOf(x), what: "removed", kind: x.kind || "breaking", before: x.sig || x.before, k: x.k });
    for (const x of d.changed || []) out.push({ path: pathOf(x), what: "changed", kind: x.kind || "breaking", before: x.before, after: x.after });
    for (const x of d.deprecated || []) out.push({ path: pathOf(x), what: "deprecated", kind: "additive" });
    for (const x of d.renamed || []) out.push({ path: pathOf(x.to || x), what: "renamed", kind: x.kind || "breaking", before: pathOf(x.from || {}), after: pathOf(x.to || {}) });
    const fv = (m, word) => { for (const [owner, v] of Object.entries(m || {})) { for (const n of v.added || []) out.push({ path: owner + "::" + (n.name || n), what: `${word} added`, kind: v.kind || (word === "variant" ? "breaking" : "additive") }); for (const n of v.removed || []) out.push({ path: owner + "::" + (n.name || n), what: `${word} removed`, kind: "breaking" }); } };
    fv(d.fields, "field"); fv(d.variants, "variant");
    return out;
  }
  // one row per change: a re-export makes the same item appear under two paths. Items fold by their
  // declared path (the release's `via`, or the owner's `via` for a member), never by a path's tail:
  // `de::Error::fmt` and `ser::Error::fmt` are two items, `toml::from_str` and `toml::de::from_str` one.
  // follow `via` to a fixed point: a member re-exported under a re-exported owner
  // (`toml::de::Deserializer::parse` → `toml::Deserializer::parse` → `toml::de::deserializer::Deserializer::parse`)
  function declared(c, path, vers) {
    const api = rel(c).api || {};
    for (const v of vers) {
      const a = api[v]; if (!a) continue;
      let p = path;
      for (let hop = 0; hop < 8; hop++) {
        if (a[p] && a[p].via && a[p].via !== p) { p = a[p].via; continue; }
        const at = p.lastIndexOf("::"); const owner = at > 0 ? a[p.slice(0, at)] : null;
        if (owner && owner.via && owner.via !== p.slice(0, at)) { p = owner.via + p.slice(at); continue; }
        break;
      }
      if (p !== path) return p;
    }
    return path;
  }
  const dedupe = (list, c, d) => { const seen = new Set(); const vers = d ? [d.to, d.from] : []; return list.filter((x) => { const t = x.what + "|" + (c ? declared(c, x.path, vers) : x.path) + "|" + (x.before || "") + "|" + (x.after || ""); if (seen.has(t)) return false; seen.add(t); return true; }); };
  // does a changed path belong to this symbol (the item itself or one of its members)?
  function mine(i, path) {
    const n = G.N[i]; const tail = path.split("::");
    const name = tail[tail.length - 1], owner = tail[tail.length - 2];
    if (name === n.n) return true;
    return owner === n.n;
  }

  // a signature in the page's plain words: "(pointer text) → maybe Value"; anything else stays as written
  const text = (html) => html.replace(/<[^>]*>/g, "").replace(/\s+/g, " ").trim();
  function parts(sig, from) { // {args: [{html, text}], ret: {html, text}} or null when it is not a fn
    const T = window.GRAPH_PAGE && window.GRAPH_PAGE.types; if (!T || !T.typeHTML || !sig || from < 0) return null;
    const m = /\bfn\s+\w+\s*(?:<[^(]*>)?\s*\(([\s\S]*)\)\s*(?:->\s*([\s\S]*?))?\s*(?:where[\s\S]*)?;?\s*$/.exec(sig);
    if (!m) return null;
    const args = T.splitTop(m[1]).filter((a) => !/^(&\s*('\w+\s+)?)?(mut\s+)?self\b/.test(a.trim()) && !/^self\s*:/.test(a.trim())).map((a) => { const c = a.indexOf(":"); const html = c < 0 ? T.typeHTML(a, from, -1) : `<i class="gtn">${esc(a.slice(0, c).trim())}</i> ${T.typeHTML(a.slice(c + 1).trim(), from, -1)}`; return { html, text: text(html) }; });
    const rh = m[2] ? T.typeHTML(m[2].trim(), from, -1) : ""; return { args, ret: { html: rh, text: text(rh) } };
  }
  // one side of a change, with what differs from the other side marked
  function side(p, other, cls) {
    const has = new Set(other.args.map((a) => a.text));
    const a = p.args.map((x) => (has.has(x.text) ? x.html : `<b class="${cls}">${x.html}</b>`)).join('<i class="gtp">, </i>');
    const r = p.ret.text ? ` <i class="gtp">→</i> ${p.ret.text === other.ret.text ? p.ret.html : `<b class="${cls}">${p.ret.html}</b>`}` : "";
    return `<span class="gsl"><i class="gtp">(</i>${a}<i class="gtp">)</i>${r}</span>`;
  }
  function words(sig, from) { const p = parts(sig, from); return p ? `<span class="gsl"><i class="gtp">(</i>${p.args.map((x) => x.html).join('<i class="gtp">, </i>')}<i class="gtp">)</i>${p.ret.text ? ` <i class="gtp">→</i> ${p.ret.html}` : ""}</span>` : esc(sig); }

  // ------------------------------------------------------------------ the lens section on a page
  function lens(i) {
    const c = crateOf(i); if (!c) return "";
    const pin = rel(c).pinned, view = viewed(c); if (view === pin) return "";
    const a = pin, b = view; const forward = cmp(pin, view) < 0;
    const d = diffOf(c, a, b);
    const head = `<h2>${forward ? "Upgrading" : "Going back"} to ${esc(short(view))}</h2>`;
    if (!d) return `<section class="csec glens">${head}<p class="rnone">${esc(short(view))} is not on this machine, so its API cannot be compared; only its date is known.</p></section>`;
    const here = dedupe(changes(d).filter((x) => mine(i, x.path)), c, d).sort((p, q) => (spellingOnly(p, i) - spellingOnly(q, i)) || ((q.kind === "breaking") - (p.kind === "breaking")));
    const imp = impactOf(c, a, b); // the crate-wide question: which of your uses does this move touch
    const sigRow = (x) => {
      const nm = x.path.split("::").pop();
      let before = x.before ? `<code class="glb" title="${esc(x.before)}">${words(x.before, i)}</code>` : "", after = x.after ? `<code class="gla" title="${esc(x.after)}">${words(x.after, i)}</code>` : "";
      const pb = x.what === "changed" && parts(x.before, i), pa = x.what === "changed" && parts(x.after, i);
      if (pb && pa) {
        if (text(side(pb, pa, "x")) === text(side(pa, pb, "x"))) return `<div class="glrow same"><span class="glw">changed</span><span class="gln">${esc(nm)}</span><span class="gls"><span class="gsame">reads the same in plain words; only its Rust spelling moved</span><code class="gla" title="${esc(x.after)}">${words(x.after, i)}</code></span></div>`;
        before = `<code class="glb" title="${esc(x.before)}">${side(pb, pa, "gold")}</code>`; after = `<code class="gla" title="${esc(x.after)}">${side(pa, pb, "gnew")}</code>`;
      }
      const word = x.what;
      return `<div class="glrow ${x.kind === "breaking" ? "br" : "ad"}"><span class="glw">${esc(word)}</span><span class="gln">${esc(nm)}</span>${x.what === "changed" ? `<span class="gls">${forward ? before + after : after + before}</span>` : `<span class="gls">${before || after}</span>`}</div>`;
    };
    const rows = here.slice(0, 8).map(sigRow).join("") || `<p class="rnone">${esc(G.N[i].n)} itself is unchanged between ${esc(a)} and ${esc(b)}.</p>`;
    const more = here.length > 8 ? `<div class="rfoot">and ${here.length - 8} more changes to it</div>` : "";
    const real = imp.filter((u) => !spellingOnly(u.change, i)), moved = imp.length - real.length;
    const cap = (u) => `<figure class="use${spellingOnly(u.change, i) ? " same" : ""}"><figcaption><span class="uw">${esc((u.file || "").split("/").slice(-2).join("/"))}:${u.line || ""}</span><span class="glw ${((u.change || {}).kind || "breaking") === "breaking" && !spellingOnly(u.change, i) ? "br" : ""}">${spellingOnly(u.change, i) ? "same in plain words" : esc(what(u.change))} · ${esc(pathOf(u.change || u).split("::").slice(-2).join("::"))}</span></figcaption><pre>${esc((u.text || "").trim())}</pre></figure>`;
    const uses = real.slice(0, 4).map(cap).join("");
    const same = imp.filter((u) => spellingOnly(u.change, i));
    const total = (rel(c).uses || []).length;
    const youLine = !total ? "" : `<div class="glyou">${real.length ? `<b>${real.length}</b> of the ${total} places your code uses ${esc(c)} change` : `none of the ${total} places your code uses ${esc(c)} change`}${moved ? `<div class="gmoved">${moved} touch${moved === 1 ? "es" : ""} ${esc(same[0] ? pathOf(same[0].change || same[0]).split("::").slice(-2).join("::") : "an item")}${new Set(same.map((u) => pathOf(u.change || u))).size > 1 ? " and others" : ""}, respelled but the same in plain words: ${same.slice(0, 4).map((u) => `<span class="gsite" title="${esc((u.text || "").trim())}">${esc((u.file || "").split("/").slice(-2).join("/"))}:${u.line}</span>`).join(", ")}${same.length > 4 ? ` and ${same.length - 4} more` : ""}</div>` : ""}</div>`;
    return `<section class="csec glens">${head}${youLine}${uses ? `<div class="gluses">${uses}</div>` : ""}<div class="glrows">${rows}</div>${more}</section>`;
  }

  // ------------------------------------------------------------------ interaction
  let rerender = null;
  function bind(root) {
    root.querySelectorAll(".vcomb").forEach((el) => {
      const c = el.dataset.crate; const vs = list(c); const ticks = [...el.querySelectorAll(".t")]; const tip = el.querySelector(".ctip");
      const dense = vs.length > W / 3;
      const place = (fish) => { const xs = positions(vs.length, fish); ticks.forEach((t, k) => (t.style.left = xs[k].toFixed(2) + "px")); return xs; };
      let xs = place(null);
      const near = (x) => { let best = 0, bd = 1e9; xs.forEach((p, k) => { const d = Math.abs(p - x); if (d < bd) { bd = d; best = k; } }); return best; };
      const show = (k) => { ticks.forEach((t) => t.classList.remove("hov")); if (k < 0) { tip.style.opacity = 0; return; } ticks[k].classList.add("hov"); const x = vs[k]; tip.innerHTML = `<b>${esc(short(x.v))}</b> · ${esc(ago(x.at))}${x.v === rel(c).pinned ? " · you pin it" : ""}${x.local ? "" : " · date only"}`; tip.style.left = xs[k] + "px"; tip.style.opacity = 1; };
      el.addEventListener("pointermove", (e) => { const x = e.clientX - el.getBoundingClientRect().left; if (dense) xs = place({ c: x, r: 90, m: 4.2 }); show(near(x)); });
      el.addEventListener("pointerleave", () => { if (dense) xs = place(null); show(-1); });
      el.addEventListener("click", (e) => { const k = near(e.clientX - el.getBoundingClientRect().left); scrub(c, vs[k].v); });
      el.addEventListener("keydown", (e) => {
        const k = vs.findIndex((x) => x.v === viewed(c));
        const to = e.key === "ArrowLeft" ? k - 1 : e.key === "ArrowRight" ? k + 1 : e.key === "Home" ? 0 : e.key === "End" ? vs.length - 1 : e.key === "Escape" ? vs.findIndex((x) => x.v === rel(c).pinned) : null;
        if (to === null) return; e.preventDefault(); e.stopPropagation(); scrub(c, vs[Math.max(0, Math.min(vs.length - 1, to))].v, true);
      });
    });
  }
  function scrub(c, v, keepFocus) {
    if (v === rel(c).pinned) state.delete(c); else state.set(c, v);
    if (rerender) rerender();
    if (keepFocus) { const el = document.querySelector(`.vcomb[data-crate="${c}"]`); if (el) el.focus(); }
  }
  window.addEventListener("keydown", (e) => { if (e.key === "Escape" && state.size && !e.target.closest?.(".vcomb")) { const c = [...state.keys()][0]; state.delete(c); if (rerender) rerender(); e.stopImmediatePropagation(); } }, true);
  function init(g, again) { G = g; rerender = again; }
  function viewing(i) { const c = G && crateOf(i); return c ? { crate: c, pin: rel(c).pinned, view: viewed(c) } : null; }
  function at(c, v) { if (rel(c)) { if (v === rel(c).pinned) state.delete(c); else state.set(c, v); } }
  // for the graph: every scrubbed crate's world nodes that the move removes or really changes, and how many it adds
  let marksKey = "", marksVal = null;
  function marks() {
    if (!G || !state.size) return null;
    const k = [...state].join(";"); if (k === marksKey) return marksVal;
    const removed = new Set(), changed = new Set(); let added = 0; const crates = [];
    for (const [c, v] of state) {
      const d = diffOf(c, rel(c).pinned, v); if (!d) continue; crates.push([c, v]);
      const anchor = anchorOf(c); const tail = (p) => p.split("::").slice(-2).join("::");
      const want = new Map(); for (const x of dedupe(changes(d), c, d)) { if (x.what === "added") { added++; continue; } if (x.what === "changed" && spellingOnly(x, anchor)) continue; want.set(tail(x.path), x.what === "removed" ? "removed" : "changed"); }
      G.N.forEach((n, i) => { if (!G.PK[n.p] || G.PK[n.p].name !== c) return; const t = (n.u >= 0 ? G.N[n.u].n + "::" : "") + n.n; const w = want.get(t) || want.get(G.qual(i).split("::").pop() + "::" + n.n); if (w === "removed") removed.add(i); else if (w) changed.add(i); });
    }
    marksKey = k; marksVal = { removed, changed, added, crates }; return marksVal;
  }
  window.GRAPH_RELEASES_UI = { init, header, lens, bind, viewing, at, marks, short: (v) => short(v) };
})();
