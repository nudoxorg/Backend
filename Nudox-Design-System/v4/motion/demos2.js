/* FACET v4 — demos2.js: moving between places (deeper, back, sibling, across, the jump bar). */
(() => {
  "use strict";
  const { C, S, spring, tween, clamp, P, E, lerp, band, keys, mixc, tf, clip, noclip, css, box, h, esc, kind, ico, chev, gem, reel, hreel, odo, stage, segs, shelfRows, demo } = window.MO;
  const { markAt } = window.DEMO_HELPERS;
  const MD = window.MD;
  const $ = (root, s) => root.querySelector(s);
  const $$ = (root, s) => [...root.querySelectorAll(s)];
  const G = Object.fromEntries(MD.glyph.map((g) => [g.n, g]));
  const MODS = MD.presentMods;

  // ------------------------------------------------------------------ shared page pieces (real present::glyph)
  const code = {
    RelationLabel: `<span class="at">#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]</span>
<span class="kw">pub enum</span> <span class="ty">RelationLabel</span> <span class="p">{</span>
    <span class="va">Typed</span><span class="p">(</span><span class="ty">SemanticLinkKind</span><span class="p">,</span> <span class="ty">RelationDirection</span><span class="p">),</span>
    <span class="va">Neighbourhood</span><span class="p">,</span>
    <span class="va">Related</span><span class="p">,</span>
<span class="p">}</span>`,
    relation_label: `<span class="kw">pub const fn</span> <span class="ca">relation_label</span><span class="p">(</span>
    kind<span class="p">:</span> <span class="ty">SemanticLinkKind</span><span class="p">,</span>
    direction<span class="p">:</span> <span class="ty">RelationDirection</span><span class="p">,</span>
<span class="p">) -&gt;</span> <span class="ty">RelationLabel</span>`,
    typed_label: `<span class="kw">const fn</span> <span class="ca">typed_label</span><span class="p">(</span>kind<span class="p">:</span> <span class="ty">SemanticLinkKind</span><span class="p">,</span> direction<span class="p">:</span> <span class="ty">RelationDirection</span><span class="p">) -&gt;</span> <span class="kw">&amp;'static</span> <span class="ty">str</span>`,
    RelationDirection: `<span class="at">#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]</span>
<span class="kw">pub enum</span> <span class="ty">RelationDirection</span> <span class="p">{</span>
    <span class="va">Outgoing</span><span class="p">,</span>
    <span class="va">Incoming</span><span class="p">,</span>
<span class="p">}</span>`,
  };
  const madeOf = {
    RelationLabel: [["variant", `Typed<span class="p">(</span><span class="t">SemanticLinkKind</span><span class="p">, </span><span class="t">RelationDirection</span><span class="p">)</span>`, "A relation whose compiler kind and direction are both known."],
      ["variant", "Neighbourhood", "A bounded neighbourhood whose per-edge kind the reply did not carry."], ["variant", "Related", "Incoming and outgoing neighbours whose per-edge kind is not carried."]],
    relation_label: [["enum", "SemanticLinkKind", "takes · the compiler's kind"], ["enum", "RelationDirection", "takes · which end you stand on"], ["enum", "RelationLabel", "gives · the readable label"]],
    typed_label: [["enum", "SemanticLinkKind", "takes"], ["enum", "RelationDirection", "takes"]],
    RelationDirection: [["variant", "Outgoing", "The read declaration is the source of the relation."], ["variant", "Incoming", "The read declaration is the target of the relation."]],
  };
  const secName = { RelationLabel: "Made of", RelationDirection: "Made of", relation_label: "Takes and gives", typed_label: "Takes" };
  const factsOf = (n) => { const g = G[n]; return { kind: g.k, line: String(g.line), uses: String(g.uses) }; };
  const rows2 = (list) => list.map(([k, nm, say]) => `<div class="crow2">${kind(k)}<span class="nm">${nm}</span><span class="say">${esc(say)}</span></div>`).join("");
  function symbolBody(n) {
    const f = factsOf(n);
    return `<div class="cfacts"><span>${f.kind} in <span class="mono">present::glyph</span></span><i class="sep">·</i><span>line <span class="mono">${f.line}</span></span><i class="sep">·</i><span>used in ${f.uses} places</span></div>
      <nav class="ctabs"><span class="on">Reference</span><span>Relations</span><span>Usage</span><span>History</span><span>Source</span><i class="bar"></i></nav>
      <div class="ccode">${code[n]}</div>
      <div class="csec"><h2>${secName[n]}</h2>${rows2(madeOf[n])}</div>`;
  }
  const shelfFor = (open, cur) => {
    const rows = [];
    for (const m of ["assemble", "budget", "call", "coverage", "drive", "dto", "fault", "glyph", "grammar", "identity", "language", "outline", "page"]) {
      rows.push({ k: "module", n: m, cur: cur === m });
      if (m === "glyph" && open) for (const g of MD.glyph) rows.push({ k: g.k, n: g.n, depth: 1, cur: cur === g.n });
    }
    return rows;
  };
  function shelfHTML(open, cur) {
    return `<div class="cup">${chev("s12", "transform:rotate(180deg)")}backend</div>
      <div class="cbook">${gem("package", 28).el.outerHTML}<div style="display:flex;flex-direction:column;gap:1px"><span class="bn">present</span><span class="bv">0.1.0</span></div></div>
      <div class="cfilter">${ico("filter", "s12")}<span>Filter</span></div>
      <div class="crows">${shelfRows(shelfFor(open, cur))}<i class="curbar" style="position:absolute;left:0;width:2px;height:18px;background:var(--mint)"></i><i class="curbg" style="position:absolute;left:0;right:0;height:28px;background:rgba(126,242,197,.05)"></i></div>`;
  }
  window.PAGES = { code, madeOf, symbolBody, shelfHTML, shelfFor, rows2, G };

  // ================================================================== deeper / back: the row becomes the page
  function descentBuild(o) {
    const st = stage(1100, 720);
    st.jb.innerHTML = segs([["package", "present"], ["module", "glyph", true]]) ;
    // the jump bar's new segment waits folded at the end
    st.jb.insertAdjacentHTML("beforeend", "");
    const find = $(st.jb, ".find");
    find.insertAdjacentHTML("beforebegin", `<span class="jslot" style="display:inline-flex;overflow:hidden;white-space:nowrap"><span class="gt">›</span><span class="jseg cur">${kind("enum")}<span class="sn">RelationLabel</span></span></span>`);
    st.addr.innerHTML = `nudox://present/glyph<span class="aslot" style="display:inline-block;overflow:hidden;vertical-align:bottom;white-space:nowrap">/RelationLabel</span>`;
    st.shelf.innerHTML = shelfHTML(true, "glyph");
    // the old page: the module, its items
    const items = MD.glyph;
    const old = h(`<div class="cfol oldp" style="position:absolute;left:0;right:0;top:0;gap:22px">
      <div class="chero ohero">${gem("module", 60).el.outerHTML}<div><div class="nm">glyph</div><div class="ld">${esc(MD.glyphLede)}</div></div></div>
      <div class="cfacts ofacts"><span>module in <span class="mono">present</span></span><i class="sep">·</i><span>6 items</span></div>
      <div class="csec"><h2 class="oh2">Items</h2>${items.map((g) => `<div class="crow2 orow" data-n="${g.n}">${kind(g.k)}<span class="nm">${esc(g.n)}</span><span class="say">${esc(g.d || " ")}</span></div>`).join("")}</div></div>`);
    const nw = h(`<div class="cfol newp" style="position:absolute;left:0;right:0;top:0;gap:22px">
      <div class="chero nhero"><span class="hg"></span><div><div class="nm ntitle">RelationLabel</div><div class="ld nlede">${esc(G.RelationLabel.d)}</div></div></div>
      <div class="nbody" style="display:flex;flex-direction:column;gap:22px">${symbolBody("RelationLabel")}</div></div>`);
    st.reader.appendChild(old); st.reader.appendChild(nw);
    const g = gem("enum", 60); $(nw, ".hg").appendChild(g.el);
    const ctx = { root: st.root, st, old, nw, g };
    ctx.after = () => {
      const R = st.reader;
      const row = $(old, `.orow[data-n="RelationLabel"]`);
      ctx.o = { row: box(row, R), mark: box($(row, ".k"), R), name: box($(row, ".nm"), R), say: box($(row, ".say"), R) };
      ctx.n = { gem: box(g.el, R), title: box($(nw, ".ntitle"), R), lede: box($(nw, ".nlede"), R), body: box($(nw, ".nbody"), R) };
      ctx.parts = [...$$(old, ".ohero,.ofacts,.oh2"), ...$$(old, ".orow")].filter((e) => e !== row).map((e) => ({ el: e, b: box(e, R) }));
      ctx.row = row;
      // shelf rows: glyph's children fold, the bar travels glyph -> RelationLabel
      const rowsEl = $(st.shelf, ".crows");
      const kids = $$(st.shelf, ".crow").filter((r) => MD.glyph.some((x) => x.n === r.dataset.n));
      ctx.kids = kids; ctx.kidH = 28 * kids.length;
      const wrap = h(`<div class="kfold" style="overflow:hidden"></div>`);
      kids[0].before(wrap); kids.forEach((k) => wrap.appendChild(k));
      ctx.kfold = wrap;
      ctx.yGlyph = box($(st.shelf, `.crow[data-n="glyph"]`), rowsEl).y;
      ctx.yRL = box($(st.shelf, `.crow[data-n="RelationLabel"]`), rowsEl).y;
      ctx.jslot = $(st.jb, ".jslot"); ctx.jW = ctx.jslot.getBoundingClientRect().width;
      ctx.aslot = $(st.addr, ".aslot"); ctx.aW = ctx.aslot.getBoundingClientRect().width;
      const glyphSeg = $$(st.jb, ".jseg")[1]; ctx.glyphSeg = glyphSeg;
      const bar = $(nw, ".ctabs .bar"); const on = $(nw, ".ctabs .on"); bar.style.width = `${on.getBoundingClientRect().width}px`;
    };
    return ctx;
  }
  /** everything is a function of one driver p (0 = the list, 1 = the page) */
  function descentFrame(c, p, t, o, dir) {
    const R = c.st.reader;
    const q = clamp(p);
    // 1. the origin row: its mark grows its stone, its name grows into the title, its sentence becomes the lede
    const m = band(q, 0, 0.6); // the driver is already a spring: bands of it stay linear
    const morph = (el, from, to, fit = "h") => {
      const s = fit === "h" ? from.h / to.h : from.w / to.w;
      const sc = lerp(s, 1, m);
      tf(el, lerp(from.x - to.x, 0, m), lerp(from.y - to.y, 0, m), sc, sc);
      el.style.transformOrigin = "0 0";
    };
    morph(c.g.el, c.o.mark, c.n.gem);
    morph($(c.nw, ".ntitle"), c.o.name, c.n.title);
    { const m0 = m; const ml = band(q, 0.1, 0.7); // the sentence trails the name, so it never crosses it
      const el = $(c.nw, ".nlede"), from = c.o.say, to = c.n.lede, sc = lerp(from.h / to.h, 1, ml);
      tf(el, lerp(from.x - to.x, 0, ml), lerp(from.y - to.y, 0, ml), sc, sc); el.style.transformOrigin = "0 0"; void m0; }
    c.g.light({ progress: 12, unlit: 0.05 });
    c.g.el.style.opacity = 1;
    // the stone's facets light as it grows: at mark size it is only its glyph
    $$(c.g.el, ".fc").forEach((f, i) => { f.style.opacity = lerp(0.0, window.MO.LIT[i], band(q, 0.1, 0.55)).toFixed(3); });
    $(c.g.el, ".rim").style.opacity = band(q, 0.1, 0.6);
    $(c.g.el, ".tb").style.opacity = band(q, 0.1, 0.6);
    // 2. the list opens around the row: everything spreads away from it (a zoom whose origin is the row)
    // every part moves away from the origin in proportion to its distance, fast enough that the nearest leaves
    const k = band(q, 0, 0.75) * 9;
    for (const part of c.parts) {
      const d = part.b.y + part.b.h / 2 - (c.o.row.y + c.o.row.h / 2);
      tf(part.el, 0, d * k);
    }
    c.row.style.visibility = q > 0.001 ? "hidden" : "visible";
    c.old.style.visibility = q >= 0.999 ? "hidden" : "visible";
    // 3. the body unfolds downward from the hero's baseline (a clip; nothing fades)
    const u = band(q, 0.55, 1); // only once the hero has landed
    const body = $(c.nw, ".nbody");
    clip(body, -24, -40, (c.n.body.h + 24) * (1 - u), -40);
    tf(body, 0, -10 * (1 - u));
    c.nw.style.visibility = q <= 0.001 ? "hidden" : "visible";
    // the new page's ground is its own: the hero parts are the only things that travel
    // 4. the shelf: glyph opens, the bar follows you down into it
    c.kfold.style.height = `${(c.kidH * band(q, 0.05, 0.6)).toFixed(1)}px`;
    const yb = lerp(c.yGlyph, c.yRL, band(q, 0.15, 0.85));
    const bar = $(c.st.shelf, ".curbar"), bg = $(c.st.shelf, ".curbg");
    bar.style.top = `${(yb + 5).toFixed(1)}px`; bg.style.top = `${yb.toFixed(1)}px`;
    $$(c.st.shelf, ".crow").forEach((r) => { r.style.color = ""; });
    // 5. the jump bar grows one segment; the address grows one word
    c.jslot.style.width = `${(c.jW * band(q, 0.45, 1)).toFixed(1)}px`;
    c.aslot.style.width = `${(c.aW * band(q, 0.45, 1)).toFixed(1)}px`;
    c.glyphSeg.classList.toggle("cur", q < 0.5);
    // reduced motion: the arrival is marked where you now are
    if (o.reduced) {
      const mk = dir > 0 ? markAt(t, 0) : 0;
      $(c.jslot, ".jseg").style.boxShadow = mk > 0 ? `inset 0 -2px 0 rgba(147,162,250,${mk})` : "";
      const back = dir < 0 ? markAt(t, 0) : 0;
      c.row.style.background = back > 0 ? `rgba(147,162,250,${0.12 * back})` : "";
    } else if (dir < 0) {
      // back: the row you left keeps a quiet tint while the list closes around it, then lets go
      const tint = P(t, 180, 60) * (1 - P(t, 700, 240));
      c.row.style.background = `rgba(147,162,250,${(0.09 * tint).toFixed(3)})`;
    }
  }
  demo({
    id: "deeper", group: "Places", title: "Deeper: the row becomes the page", relation: "deeper (open a row)",
    sentence: "The row you open is the page you get: its mark grows its stone, its name grows into the title, its sentence becomes the lede, and the list spreads away from it, so you always know where you came from.",
    w: 1100, h: 720, dur: 700, film: [0, 30, 60, 90, 120, 150, 190, 240, 300, 420], crop: [0, 0, 1100, 560], interrupt: "back at 170 ms",
    spec: [["Driver", "one track <code>p</code> 0 → 1, spring <b>CARRY</b> (response .34 s, ζ 1: ~360 ms, no overshoot); every pose below is a band of <code>p</code>, so Back and interruption are the same choreography run the other way"],
      ["Shared (become)", "mark → hero gem, name → title (<code>p</code> 0–.6), sentence → lede (.1–.7, trailing): <code>motion::shared</code>, <code>Fit::Height</code>; the stone's facets light .1–.55"],
      ["Spread", "every other element moves away from the origin row by <code>(y − y₀) · 9 · p</code> (p 0–.75): a zoom whose origin is the row; the nearest neighbour leaves through the reader's edge first-fastest; no scale, no fade"],
      ["Unfold", "once the hero has landed, the body is revealed downward from its baseline, <code>p</code> .55–1, clip + 10 px drop"],
      ["Shelf / jump bar", "glyph's children unroll (height, .05–.6), the mint bar follows (.15–.85); the jump bar and address grow one segment (.45–1)"],
      ["Reduced", "cut; the new jump-bar segment holds a periwinkle mark for 1.2 s"],
      ["GPUI", "<code>shared</code> + <code>Offset</code> + <code>Reveal</code>; missing: <b>Shared driven by an external progress</b> (so descent and back retarget one track)"]],
    build: descentBuild,
    frame(c, t, o) {
      const p = o.reduced ? (t > 0 ? 1 : 0) : spring(0, o.int ? [[0, 1, S.carry], [170, 0, S.carry]] : [[0, 1, S.carry]], S.carry)(t);
      descentFrame(c, p, t, o, o.int && t > 170 ? -1 : 1);
    },
  });
  demo({
    id: "back", group: "Places", title: "Back: out the way you came", relation: "back out",
    sentence: "Back replays the arrival in reverse: the body folds into the hero, the hero shrinks home into its row, and the list closes around it, which keeps a quiet tint for a moment: this is where you were.",
    w: 1100, h: 720, dur: 1000, film: [0, 40, 80, 120, 160, 200, 250, 320, 450, 900], crop: [0, 0, 1100, 560],
    spec: [["Rule", "every history entry remembers the relation that brought you (deeper, sibling, across, find); Back plays that relation's motion with its driver reversed, so the path home is the path you took"],
      ["Timing", "the same <code>p</code> 1 → 0 on CARRY: body folds first (p 1–.35), then the hero flies home (.8–0), then the list closes (.7–0)"],
      ["Mark", "the row you came from holds a periwinkle tint (0.09) from 180 ms to 700 ms, then lets go over 240 ms: colour only"],
      ["GPUI", "missing: <b>arrival relation on the history entry</b> (navigation) so Back can pick the reverse"]],
    build: descentBuild,
    frame(c, t, o) {
      const p = o.reduced ? (t > 0 ? 0 : 1) : spring(1, [[0, 0, S.carry]], S.carry)(t);
      descentFrame(c, p, t, o, -1);
    },
  });

  // ================================================================== sibling: the reel
  const SIB = ["RelationDirection", "RelationLabel", "relation_label", "typed_label"];
  demo({
    id: "sibling", group: "Places", title: "Sibling: the reel", relation: "sibling (next or previous)",
    sentence: "Next is the one below: the name, the lede and the stone's glyph sit on a reel in list order and roll to the next one; what the siblings share (the stone, the tabs, the module) never moves, and stepping fast spins the reel past what you skip.",
    w: 1100, h: 720, dur: 800, film: [0, 40, 80, 120, 150, 180, 220, 270, 330, 450, 600], crop: [0, 0, 1100, 560],
    spec: [["Reel", "labels stacked in list order in a window the height of one; <code>pos</code> springs on <b>REEL</b> (response .24, ζ .92) to the index; each ⌥↓ retargets it (velocity kept), so two quick steps spin through the one between"],
      ["Holds", "the stone (same place, same depth: only its hue tweens and its glyph rolls inside the table), the module, the tabs, the section order"],
      ["Rolls", "differing facts tokens roll in the same direction (kind word, line number spins as a count)"],
      ["Body", "folds up into the hero (clip, first 40 %) and unfolds the next (last 60 %); never two bodies at once"],
      ["Shelf", "the mint bar travels the rows on the same spring"],
      ["GPUI", "missing: <b>Roll</b> (a clip window + Offset on a keyed column); the rest is <code>Reveal</code> and colour"]],
    build(o) {
      const st = stage(1100, 720);
      st.jb.innerHTML = segs([["package", "present"], ["module", "glyph"], ["enum", "RelationLabel", true]]);
      st.addr.textContent = "nudox://present/glyph/RelationLabel";
      st.shelf.innerHTML = shelfHTML(true, "RelationLabel");
      const names = SIB;
      const R = st.reader;
      R.innerHTML = `<div class="cfol" style="gap:22px">
        <div class="chero"><span class="hg"></span><div><div class="nm"><span class="rtitle"></span></div><div class="ld"><span class="rlede"></span></div></div></div>
        <div class="cfacts"><span><span class="rkind"></span> in <span class="mono">present::glyph</span></span><i class="sep">·</i><span>line <span class="mono rline"></span></span><i class="sep">·</i><span>used in <span class="ruses"></span> places</span></div>
        <nav class="ctabs"><span class="on">Reference</span><span>Relations</span><span>Usage</span><span>History</span><span>Source</span><i class="bar"></i></nav>
        <div class="sbody">${names.map((n) => `<div class="sb" data-n="${n}" style="display:none;flex-direction:column;gap:22px"><div class="ccode">${code[n]}</div><div class="csec"><h2>${secName[n]}</h2>${rows2(madeOf[n])}</div></div>`).join("")}</div></div>`;
      const title = reel(names.map(esc), "", null), lede = reel(names.map((n) => esc(G[n].d || " ")));
      $(R, ".rtitle").appendChild(title.el); $(R, ".rlede").appendChild(lede.el);
      const kindO = odo().states(names.map((n) => G[n].k), "version");
      const lineO = odo().states(names.map((n) => String(G[n].line)), "count");
      const usesO = odo().states(names.map((n) => String(G[n].uses)), "count");
      $(R, ".rkind").appendChild(kindO.el); $(R, ".rline").appendChild(lineO.el); $(R, ".ruses").appendChild(usesO.el);
      const g = gem("enum", 60, { glyphs: names.map((n) => G[n].k) }); $(R, ".hg").appendChild(g.el);
      const tseg = $$(st.jb, ".jseg")[2]; const segReel = reel(names.map(esc)); $(tseg, ".sn").replaceWith(segReel.el);
      const ctx = { root: st.root, st, title, lede, kindO, lineO, usesO, g, segReel };
      ctx.after = () => {
        const rowsEl = $(st.shelf, ".crows");
        ctx.ys = names.map((n) => box($(st.shelf, `.crow[data-n="${n}"]`), rowsEl).y);
        ctx.bodies = $$(R, ".sb"); ctx.bodies.forEach((b) => { b.style.display = "flex"; }); ctx.bh = ctx.bodies.map((b) => b.getBoundingClientRect().height);
        ctx.bodies.forEach((b) => { b.style.display = "none"; });
        const bar = $(R, ".ctabs .bar"); bar.style.width = `${$(R, ".ctabs .on").getBoundingClientRect().width}px`;
      };
      return ctx;
    },
    frame(c, t, o) {
      const start = 1; // RelationLabel
      const pos = o.reduced ? (t < 0 ? start : t < 140 ? start + 1 : start + 2) : spring(start, [[0, start + 1, S.reel], [140, start + 2, S.reel]], S.reel)(t);
      c.title.set(pos); c.lede.set(pos); c.segReel.set(pos);
      c.kindO.set(pos); c.lineO.set(pos); c.usesO.set(pos);
      c.g.glyph(pos);
      const fam = (n) => ({ enum: "#5fe0b4", function: "#8fa6ff" })[G[n].k] || "#5fe0b4";
      const i = clamp(Math.floor(pos), 0, SIB.length - 1), j = Math.min(i + 1, SIB.length - 1);
      c.g.color(mixc(fam(SIB[i]), fam(SIB[j]), pos - i));
      c.g.light({});
      // the body folds into the hero and the next unfolds: which body shows is the nearest whole step
      const f = pos - Math.floor(pos), near = Math.round(pos), fold = f < 0.4 ? 1 - f / 0.4 : (f - 0.4) / 0.6;
      c.bodies.forEach((b, k) => { b.style.display = k === (f < 0.4 ? Math.floor(pos) : Math.ceil(pos)) ? "flex" : "none"; });
      const shown = c.bodies.find((b) => b.style.display === "flex");
      if (shown) { const H = c.bh[c.bodies.indexOf(shown)]; const u = Math.abs(pos - near) < 0.002 ? 1 : C.glide(clamp(fold)); clip(shown, 0, -40, H * (1 - u), -40); }
      // shelf bar
      const y = lerp(c.ys[i], c.ys[j], pos - i);
      $(c.st.shelf, ".curbar").style.top = `${(y + 5).toFixed(1)}px`; $(c.st.shelf, ".curbg").style.top = `${y.toFixed(1)}px`;
      $$(c.st.shelf, ".crow.cur").forEach((r) => r.classList.remove("cur"));
      if (o.reduced) { const mk = markAt(t, 140); $(c.st.reader, ".chero .nm").style.boxShadow = mk > 0 ? `inset 0 -2px 0 rgba(147,162,250,${mk})` : ""; }
    },
  });

  // ================================================================== across: new ground, same idea
  demo({
    id: "across", group: "Places", title: "Across: new ground", relation: "across (another package)",
    sentence: "Crossing to another package moves the ground: the page is pushed the way that package lies in the world graph, the ground slides under it at a quarter of the speed, the stone turns to catch a new light, and anything the two places share (here, the name Value) holds still.",
    w: 1100, h: 720, dur: 700, film: [0, 30, 60, 90, 120, 160, 200, 250, 320, 450], crop: [0, 0, 1100, 560],
    spec: [["Direction", "the unit vector from <code>toml</code> to <code>serde_json</code> in the world graph's layout (east, slightly north); the page pushes along its x sign, the ground along the true vector"],
      ["Push", "full reader width, 320 ms glide; old and new side by side, never overlapping; the shelf rows push the same way inside the shelf"],
      ["Holds", "tokens equal on both sides (the title <code>Value</code>, the kind, the jump bar's <code>value › Value</code>) stay in place over the moving page"],
      ["Turn", "the package stone and the hero stone turn one quarter (their light map shifts three facets) over the push; geometry never rotates"],
      ["Ground", "the faceted ground translates 90 px along the world vector (parallax)"],
      ["GPUI", "<code>Offset</code> + content masks; missing: <b>Gem::turn</b> (light-map rotation, paint-level) and a <b>Ground offset</b>"]],
    build(o) {
      const st = stage(1100, 720);
      st.jb.innerHTML = segs([["package", "toml"], ["module", "value"], ["enum", "Value", true]]);
      const pseg = $$(st.jb, ".jseg")[0]; const pReel = hreel(["toml", "serde_json"]); $(pseg, ".sn").replaceWith(pReel.el);
      st.addr.textContent = "nudox://serde_json/value/Value";
      const VT = MD.valueToml, VJ = MD.valueJson;
      const tv = VT.kids.filter((k) => k.k === "variant").map((v) => [v.n, ({ String: "text", Integer: "i64", Float: "f64", Boolean: "bool", Datetime: "Datetime", Array: "Array", Table: "Table" })[v.n], v.d]);
      const jv = VJ.variants;
      const page = (vs, lede, where, uses) => `<div class="cfol" style="gap:22px;padding-top:0"><div style="height:${60}px"></div>
        <div class="ld pl" style="font:italic 400 17px/1.4 var(--serif);color:var(--ink2);margin:-22px 0 0 80px">${esc(lede)}</div>
        <div class="cfacts" style="margin-top:0">${where}</div>
        <div class="csec oneof"><div class="cgrp" style="padding-top:0">one of</div>${vs.map(([n, t2, d]) => `<div class="ov"><span>${esc(n)}</span><span class="ty2">${esc(t2)}</span><span class="dsc">${esc(d)}</span></div>`).join("")}</div></div>`;
      st.reader.innerHTML = `<div class="pgA" style="position:absolute;inset:0;top:44px">${page(tv, VT.d, `<span>enum in <span class="mono">toml::value</span></span><i class="sep">·</i><span>used in 59 places</span><i class="sep">·</i><span><b>7</b> in your code</span>`)}</div>
        <div class="pgB" style="position:absolute;inset:0;top:44px">${page(jv, VJ.d, `<span>enum in <span class="mono">serde_json::value</span></span><i class="sep">·</i><span>used in ${VJ.uses} places</span>`)}</div>
        <div class="cfol" style="position:absolute;left:0;right:0;top:0;pointer-events:none"><div class="chero"><span class="hg"></span><div><div class="nm">Value</div><div class="ld" style="visibility:hidden">.</div></div></div></div>`;
      const g = gem("enum", 60); $(st.reader, ".hg").appendChild(g.el);
      const shelfA = `<div class="cup">${chev("s12", "transform:rotate(180deg)")}dependencies</div>`;
      const rowsA = shelfRows([{ k: "module", n: "value" }, { k: "type", n: "Array", depth: 1 }, { k: "enum", n: "Value", depth: 1, cur: 1 }, { k: "macro", n: "impl_into_value", depth: 1 }, { k: "trait", n: "Index", depth: 1 }, { k: "struct", n: "SeqDeserializer", depth: 1 }, { k: "struct", n: "MapDeserializer", depth: 1 }, { k: "struct", n: "ValueSerializer", depth: 1 }]);
      const rowsB = shelfRows([{ k: "module", n: "value" }, { k: "enum", n: "Value", depth: 1, cur: 1 }, { k: "struct", n: "Number", depth: 1 }, { k: "struct", n: "Map", depth: 1 }, { k: "function", n: "to_value", depth: 1 }, { k: "function", n: "from_value", depth: 1 }, { k: "struct", n: "Serializer", depth: 1 }]);
      const pg = gem("package", 28);
      st.shelf.innerHTML = `${shelfA}<div class="cbook"><span class="pg"></span><div style="display:flex;flex-direction:column;gap:1px"><span class="bn preel"></span><span class="bv vreel"></span></div></div>
        <div class="cfilter">${ico("filter", "s12")}<span>Filter</span></div>
        <div style="position:relative;overflow:hidden;height:260px"><div class="crows rA" style="position:absolute;left:0;right:0">${rowsA}</div><div class="crows rB" style="position:absolute;left:0;right:0">${rowsB}</div></div>`;
      $(st.shelf, ".pg").appendChild(pg.el);
      const nReel = hreel(["toml", "serde_json"]), vReel = hreel(["0.8.23", "1.0.151"]);
      $(st.shelf, ".preel").appendChild(nReel.el); $(st.shelf, ".vreel").appendChild(vReel.el);
      $$(st.shelf, ".crow.cur").forEach((r) => { r.style.background = "rgba(126,242,197,.05)"; r.insertAdjacentHTML("afterbegin", `<i style="position:absolute;left:0;top:5px;bottom:5px;width:2px;background:var(--mint)"></i>`); });
      const a = MD.geo.toml, b = MD.geo.serde_json; const dx = b[0] - a[0], dy = b[1] - a[1], L = Math.hypot(dx, dy);
      return { root: st.root, st, g, pg, pReel, nReel, vReel, dir: [dx / L, dy / L] };
    },
    frame(c, t, o) {
      const q = o.reduced ? (t > 0 ? 1 : 0) : E(t, 0, 320, C.glide);
      const W = c.st.reader.getBoundingClientRect().width, sx = Math.sign(c.dir[0]) || 1;
      tf($(c.st.reader, ".pgA"), -sx * W * q, 0); tf($(c.st.reader, ".pgB"), sx * W * (1 - q), 0);
      const SW = 240;
      tf($(c.st.shelf, ".rA"), -sx * SW * q, 0); tf($(c.st.shelf, ".rB"), sx * SW * (1 - q), 0);
      // names roll sideways, in the same direction as the push
      [c.nReel, c.vReel, c.pReel].forEach((r) => r.set(q));
      // the ground moves under the page along the true world vector
      tf(c.st.ground, -c.dir[0] * 90 * q, -c.dir[1] * 90 * q);
      // stones turn a quarter: their light moves three facets
      const turn = o.reduced ? 0 : 3 * C.glide(q);
      c.g.light({ turn }); c.pg.light({ turn });
      if (o.reduced) { const mk = markAt(t, 0); $(c.st.shelf, ".cbook").style.boxShadow = mk > 0 ? `inset 2px 0 0 rgba(147,162,250,${mk})` : ""; }
    },
  });

  // ================================================================== the jump bar: siblings, where you are
  demo({
    id: "jump", group: "Places", title: "The jump bar", relation: "sideways, from where you are",
    sentence: "A segment of the path opens its siblings as a menu that grows down out of the segment itself, with where you are already marked; choosing one is an ordinary sibling move, and the menu rolls back into the segment as the name rolls to your choice.",
    w: 1100, h: 520, dur: 1300, film: [0, 50, 100, 160, 240, 480, 540, 600, 820, 880, 950, 1150], crop: [180, 0, 740, 380],
    spec: [["Open", "the segment's plate extends downward into the menu (its left edge is the menu's left edge); rows are revealed by a clip, 200 ms glide; the current row already carries the periwinkle bevel"],
      ["Move", "↑/↓: the bevel travels (focus travel, SNAPPY) — never two bevels"],
      ["Choose", "the menu rolls back up into the segment (120 ms, accelerate) while the segment's name rolls to the choice on the list-order reel; the page does the sibling reel"],
      ["Back menu", "a long press on ‹ drops the last ten places the same way, most recent nearest the button"],
      ["GPUI", "float layer <code>FloatKind::Menu</code> with an <b>unroll</b> entrance (missing: clip act) and the focus ring as one <code>shared</code> element"]],
    build(o) {
      const st = stage(1100, 520);
      st.jb.innerHTML = segs([["package", "present"], ["module", "glyph"], ["enum", "RelationLabel", true]]);
      st.addr.textContent = "nudox://present/glyph/RelationLabel";
      st.shelf.innerHTML = shelfHTML(true, "RelationLabel");
      st.reader.innerHTML = `<div class="cfol" style="gap:22px"><div class="chero"><span class="hg"></span><div><div class="nm"><span class="rt"></span></div><div class="ld"><span class="rl2"></span></div></div></div></div>`;
      const names = MD.glyph.map((g) => g.n);
      const g = gem("enum", 60, { glyphs: names.map((n) => G[n].k) }); $(st.reader, ".hg").appendChild(g.el);
      const title = reel(names.map(esc)), lede = reel(names.map((n) => esc(G[n].d || "—")));
      $(st.reader, ".rt").appendChild(title.el); $(st.reader, ".rl2").appendChild(lede.el);
      const seg = $$(st.jb, ".jseg")[2]; const sreel = reel(names.map(esc)); $(seg, ".sn").replaceWith(sreel.el);
      const menu = h(`<div class="jmenu" style="left:0;top:0">${names.map((n) => `<div class="jmi${n === "RelationLabel" ? " jhere" : ""}">${kind(G[n].k)}<span>${esc(n)}</span></div>`).join("")}<i class="sel"></i></div>`);
      st.win.appendChild(menu);
      const ctx = { root: st.root, st, g, title, lede, sreel, menu, seg, names };
      ctx.after = () => {
        const sb = box(seg, st.win); ctx.sb = sb;
        menu.style.left = `${sb.x}px`; menu.style.top = `${sb.y + sb.h}px`;
        ctx.mh = menu.getBoundingClientRect().height;
        ctx.cur = names.indexOf("RelationLabel");
      };
      return ctx;
    },
    frame(c, t, o) {
      const open = o.reduced ? (t >= 0 && t < 820 ? 1 : 0) : tween(0, [[0, 1, 200, C.glide], [820, 0, 120, C.drop]])(t);
      clip(c.menu, 0, -20, c.mh * (1 - open), -20);
      tf(c.menu, 0, -6 * (1 - open));
      c.menu.style.visibility = open <= 0.001 ? "hidden" : "visible";
      // the segment's plate joins the menu while it is open
      c.seg.style.background = open > 0.01 ? `rgba(147,162,250,${(0.1 * open).toFixed(3)})` : "";
      const sel = o.reduced ? (t < 480 ? c.cur : c.cur + 1) : spring(c.cur, [[480, c.cur + 1, S.snappy]], S.snappy)(t);
      $(c.menu, ".sel").style.top = `${(5 + sel * 26).toFixed(1)}px`;
      const pos = o.reduced ? (t < 820 ? c.cur : c.cur + 1) : spring(c.cur, [[820, c.cur + 1, S.reel]], S.reel)(t);
      c.sreel.set(pos); c.title.set(pos); c.lede.set(pos); c.g.glyph(pos);
      const fam = (n) => ({ enum: "#5fe0b4", function: "#8fa6ff", struct: "#5fe0b4" })[G[n].k];
      const i = Math.floor(pos), j = Math.min(i + 1, c.names.length - 1);
      c.g.color(mixc(fam(c.names[i]), fam(c.names[j]), pos - i)); c.g.light({});
    },
  });
})();
