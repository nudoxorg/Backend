/* FACET v4 — demos3.js: things that open (peek, chain, lens, fold, x-ray) and lists that change (find, add),
 * and taking things (copy, the hand). */
(() => {
  "use strict";
  const { C, S, spring, tween, clamp, P, E, lerp, band, keys, mixc, tf, clip, noclip, css, box, h, esc, kind, ico, chev, gem, reel, hreel, odo, stage, segs, shelfRows, demo } = window.MO;
  const { markAt } = window.DEMO_HELPERS;
  const MD = window.MD;
  const $ = (root, s) => root.querySelector(s);
  const $$ = (root, s) => [...root.querySelectorAll(s)];
  const G = window.PAGES.G;
  const SEM = MD.semantic;

  /** A card that unfurls from a token: the token's underline draws, stretches into the card's top edge, and the
   *  body unrolls down from that edge (a clip). Returns handles; the demo drives the numbers. */
  function card(inner, w) {
    return h(`<div class="cwrap mcard" style="left:0;top:0;width:${w}px"><div class="ccard" style="width:${w}px">${inner}</div></div>`);
  }
  const edge = () => h(`<i class="medge" style="position:absolute;height:1.5px;background:var(--peri);left:0;top:0;width:10px;z-index:61"></i>`);
  const uline = () => h(`<i class="muline" style="position:absolute;height:1.5px;background:var(--peri);transform-origin:0 50%;z-index:2"></i>`);
  const semSig = `<span class="kw">pub enum</span> <span class="ty">SemanticLinkKind</span> <span class="p">{</span> <span class="va">Calls</span><span class="p">,</span> <span class="va mc">MethodCall</span><span class="p">,</span> <span class="p">…</span> <span class="p">}</span>`;
  const peekSem = () => `<div class="ch">${gem("enum", 28).el.outerHTML}<div><div class="nm">SemanticLinkKind</div><div class="wh">enum in <code>library::surface</code></div></div></div>
    <div class="sg">${semSig}</div><p class="sy">${esc(SEM.d)}</p><div class="fc">used in <b>${SEM.uses}</b> places</div>`;
  const peekDir = () => `<div class="ch">${gem("enum", 28).el.outerHTML}<div><div class="nm">RelationDirection</div><div class="wh">enum in <code>present::glyph</code></div></div></div>
    <div class="sg"><span class="kw">pub enum</span> <span class="ty">RelationDirection</span> <span class="p">{</span> <span class="va">Outgoing</span><span class="p">,</span> <span class="va">Incoming</span> <span class="p">}</span></div><p class="sy">${esc(G.RelationDirection.d)}</p><div class="fc">used in <b>${G.RelationDirection.uses}</b> places</div>`;
  /** a horizontal wipe between two layers: the boundary travels against the direction of reading travel */
  function wipe(a, b, w, W, dir = 1) {
    const x = dir > 0 ? W * (1 - w) : W * w; // boundary
    if (dir > 0) { clip(a, -30, W - x, -30, -30); clip(b, -30, -30, -30, x); tf(a, -12 * w, 0); tf(b, 12 * (1 - w), 0); }
    else { clip(b, -30, W - x, -30, -30); clip(a, -30, -30, -30, x); tf(a, 12 * w, 0); tf(b, -12 * (1 - w), 0); }
  }

  // ================================================================== peek
  demo({
    id: "peek", group: "Opening", title: "Peek: the word, opened", relation: "peek (rest on a word)",
    sentence: "A peek grows out of the exact token you rest on: its underline draws, stretches into the card's top edge, and the card unrolls down from that edge. Moving to the next word slides the same card over and wipes its content in reading order; leaving rolls it back up into the word.",
    w: 900, h: 480, dur: 1700, film: [0, 60, 120, 180, 240, 330, 700, 760, 830, 1300, 1400, 1520], crop: [80, 60, 820, 380],
    spec: [["Grow", "underline draws 0–90 ms; the underline becomes the card's top edge: its x and width travel to the card's (60–200 ms glide) and it stays as the card's periwinkle hairline; the body unrolls from it (clip, 140–320 ms glide). Content is never scaled and never faded"],
      ["Warm sweep", "resting on another trigger of the same kind: the card's x springs (SNAPPY) to the new anchor, its height springs to the new content, and the content wipes in reading order (160 ms): the boundary travels against the movement, old content leaves 12 px, new arrives from 12 px"],
      ["Leave", "the body rolls up into the edge (120 ms, accelerate), the edge shrinks back to the word (90 ms), the underline undraws (90 ms)"],
      ["Reduced", "the card cuts in and out; the token keeps its underline while the card is open"],
      ["Replaces", "today's float entrance (group fade + 6 px rise + 0.97 scale in <code>overlay/float.rs</code>) and its warm-swap cross-fade (<code>draw.swap</code> opacity)"],
      ["GPUI", "float layer: missing an <b>unfurl</b> entrance (card bounds morph from the anchor's underline + <code>Reveal</code> from the top) and a <b>wipe</b> swap (two content masks)"]],
    build() {
      const st = stage(900, 480, { shelf: false });
      st.jb.innerHTML = window.MO.segs([["package", "present"], ["module", "glyph"], ["enum", "RelationLabel", true]]);
      st.addr.textContent = "nudox://present/glyph/RelationLabel";
      st.reader.innerHTML = `<div class="cfol" style="padding-top:34px"><div class="csec"><h2>Made of</h2>${window.PAGES.rows2(window.PAGES.madeOf.RelationLabel)}</div></div>`;
      const t1 = $$(st.reader, ".crow2 .t")[0], t2 = $$(st.reader, ".crow2 .t")[1];
      const W = 392;
      const cd = card(`<div class="lay la" style="display:flex;flex-direction:column;gap:10px">${peekSem()}</div><div class="lay lb" style="position:absolute;left:16px;right:16px;top:14px;display:flex;flex-direction:column;gap:10px">${peekDir()}</div>`, W);
      st.win.appendChild(cd);
      const e = edge(); st.win.appendChild(e);
      const u1 = uline(), u2 = uline(); st.win.appendChild(u1); st.win.appendChild(u2);
      const ctx = { root: st.root, st, cd, e, u1, u2, t1, t2, W };
      ctx.after = () => {
        ctx.b1 = box(t1, st.win); ctx.b2 = box(t2, st.win);
        [[u1, ctx.b1], [u2, ctx.b2]].forEach(([u, b]) => { u.style.left = `${b.x}px`; u.style.top = `${b.y + b.h - 1}px`; u.style.width = `${b.w}px`; });
        const la = $(cd, ".la"), lb = $(cd, ".lb");
        ctx.hA = $(cd, ".ccard").getBoundingClientRect().height;
        lb.style.position = "static"; la.style.display = "none"; ctx.hB = $(cd, ".ccard").getBoundingClientRect().height; la.style.display = "flex"; lb.style.position = "absolute";
        ctx.la = la; ctx.lb = lb;
      };
      return ctx;
    },
    frame(c, t, o) {
      const R = o.reduced;
      const x1 = c.b1.x - 14, x2 = c.b2.x - 14, y = c.b1.y + c.b1.h + 8;
      const cx = R ? (t < 700 ? x1 : x2) : spring(x1, [[700, x2, S.snappy]], S.snappy)(t);
      const H = R ? (t < 700 ? c.hA : c.hB) : spring(c.hA, [[700, c.hB, S.snappy]], S.snappy)(t);
      // underlines: the word you rest on lights
      const d1 = R ? (t < 700 ? 1 : 0) : tween(0, [[0, 1, 90, C.glide], [700, 0, 90, C.drop]])(t);
      const d2 = R ? (t >= 700 && t < 1300 ? 1 : 0) : tween(0, [[700, 1, 90, C.glide], [1510, 0, 90, C.drop]])(t);
      c.u1.style.transform = `scaleX(${d1.toFixed(3)})`; c.u2.style.transform = `scaleX(${d2.toFixed(3)})`;
      // the edge: from the word's underline to the card's top, and back
      const grow = R ? (t < 1300 ? 1 : 0) : tween(0, [[60, 1, 140, C.glide], [1420, 0, 90, C.drop]])(t);
      const unroll = R ? (t < 1300 ? 1 : 0) : tween(0, [[140, 1, 180, C.glide], [1300, 0, 120, C.drop]])(t);
      const anchor = t < 700 ? c.b1 : c.b2;
      const ex = lerp(anchor.x, cx, grow), ew = lerp(anchor.w, c.W, grow), ey = lerp(anchor.y + anchor.h - 1, y, grow);
      css(c.e, { left: `${ex}px`, top: `${ey}px`, width: `${ew}px`, opacity: grow > 0.001 || unroll > 0 ? 1 : 0 });
      c.cd.style.left = `${cx}px`; c.cd.style.top = `${y}px`;
      $(c.cd, ".ccard").style.height = `${H}px`;
      clip(c.cd, 0, -40, (H + 30) * (1 - unroll) - 30 * (1 - unroll), -40);
      if (unroll <= 0.001) clip(c.cd, 0, 0, H + 60, 0);
      c.cd.style.visibility = unroll > 0.001 ? "visible" : "hidden";
      // warm sweep: the content wipes in reading order
      const w = R ? (t < 700 ? 0 : 1) : tween(0, [[700, 1, 160, C.glide]])(t);
      wipe(c.la, c.lb, w, c.W, 1);
    },
  });

  // ================================================================== chain
  demo({
    id: "chain", group: "Opening", title: "Chain: a peek inside a peek", relation: "peek, one deeper",
    sentence: "Resting on a word inside a card grows the next card out of the card's side, level with that word: a hairline leaves the edge, becomes the child's left edge, and the child unrolls to the right; focus moves with it. Esc rolls it back into the edge.",
    w: 1000, h: 440, dur: 1300, film: [0, 60, 120, 180, 240, 300, 380, 500, 900, 960, 1040, 1150], crop: [0, 60, 900, 330],
    spec: [["Grow", "underline 0–90 ms; connector 14 px out of the parent's edge at the word's height (60–160); the child's left edge grows up and down from there to the child's height (140–240); the child unrolls rightwards (clip, 200–400 glide)"],
      ["Focus", "the doubled periwinkle bevel leaves the parent and arrives on the child (colour, 220–380): one focus"],
      ["Back (Esc)", "the reverse, each stage 60–120 ms, accelerating"],
      ["GPUI", "float layer chain: same <b>unfurl</b> with a horizontal axis; the bevel is <code>Edge::mix(rest, focus, f)</code> already"]],
    build() {
      const st = stage(1000, 440, { shelf: false });
      st.jb.innerHTML = window.MO.segs([["package", "present"], ["module", "glyph"], ["enum", "RelationLabel", true]]);
      st.reader.innerHTML = `<div class="cfol" style="padding-top:34px;margin:0"><div class="csec"><h2>Made of</h2>${window.PAGES.rows2(window.PAGES.madeOf.RelationLabel)}</div></div>`;
      const W = 392, CW = 340;
      const parent = card(peekSem(), W); st.win.appendChild(parent);
      const mc = MD.semantic.kids.find((k) => k.n === "MethodCall");
      const child = card(`<div class="crumb">SemanticLinkKind<span class="gt">›</span>MethodCall</div><div class="ch">${gem("variant", 28).el.outerHTML}<div><div class="nm">MethodCall</div><div class="wh">variant, no payload</div></div></div><p class="sy">${esc(mc.d)}</p>`, CW);
      st.win.appendChild(child);
      const conn = h(`<i style="position:absolute;height:1px;background:var(--line3);transform-origin:0 50%;z-index:62"></i>`); st.win.appendChild(conn);
      const ledge = h(`<i style="position:absolute;width:1.5px;background:var(--peri);z-index:62"></i>`); st.win.appendChild(ledge);
      const u = uline(); st.win.appendChild(u);
      const tok = $$(st.reader, ".crow2 .t")[0];
      const ctx = { root: st.root, st, parent, child, conn, ledge, u, W, CW, tok };
      ctx.after = () => {
        const b = box(tok, st.win); ctx.px = b.x - 14; ctx.py = b.y + b.h + 8;
        parent.style.left = `${ctx.px}px`; parent.style.top = `${ctx.py}px`;
        const m = box($(parent, ".mc"), st.win); ctx.m = m;
        u.style.left = `${m.x}px`; u.style.top = `${m.y + m.h - 1}px`; u.style.width = `${m.w}px`;
        ctx.cx = ctx.px + W + 14; ctx.cy = m.y - 40;
        child.style.left = `${ctx.cx}px`; child.style.top = `${ctx.cy}px`;
        ctx.ch = $(child, ".ccard").getBoundingClientRect().height;
        conn.style.left = `${ctx.px + W}px`; conn.style.top = `${m.y + m.h / 2}px`; conn.style.width = "14px";
        ledge.style.left = `${ctx.cx}px`;
      };
      return ctx;
    },
    frame(c, t, o) {
      const R = o.reduced;
      const tw = (a, b, d1, d2) => (R ? (t < 900 ? 1 : 0) : tween(0, [[a, 1, d1, C.glide], [b, 0, d2, C.drop]])(t));
      const un = tw(0, 1080, 90, 60), cn = tw(60, 1020, 100, 60), le = tw(140, 960, 100, 60), body = tw(200, 900, 200, 120), foc = tw(220, 900, 160, 120);
      c.u.style.transform = `scaleX(${un.toFixed(3)})`;
      c.conn.style.transform = `scaleX(${cn.toFixed(3)})`;
      const mid = c.m.y + c.m.h / 2;
      const top = lerp(mid, c.cy, le), bot = lerp(mid, c.cy + c.ch, le);
      css(c.ledge, { top: `${top}px`, height: `${Math.max(0, bot - top)}px`, opacity: le > 0.001 ? 1 : 0 });
      clip(c.child, -30, (c.CW + 30) * (1 - body), -40, 0);
      c.child.style.visibility = body > 0.001 ? "visible" : "hidden";
      const pc = $(c.parent, ".ccard"), cc = $(c.child, ".ccard");
      const bev = (f) => `inset ${lerp(1, 2, f)}px ${lerp(1, 2, f)}px 0 ${mixc("#34405a", "#bcc6ff", f)},inset -${lerp(1, 2, f)}px -${lerp(1, 2, f)}px 0 ${mixc("#000000", "#93a2fa", f)}`;
      pc.style.boxShadow = bev(1 - foc); cc.style.boxShadow = bev(foc);
    },
  });

  // ================================================================== lens from a comb tick
  demo({
    id: "lens", group: "Opening", title: "Lens: a release, opened from its tick", relation: "lens (rest on a tick)",
    sentence: "A release lens grows out of its tick: the tick rises, extends down into a hairline, and the card opens from the hairline's end; sliding along the comb carries the card with the pointer and wipes its content in time's direction.",
    w: 760, h: 460, dur: 1300, film: [0, 50, 100, 150, 200, 280, 380, 700, 760, 820, 900, 1100], crop: [0, 50, 700, 360],
    spec: [["Tick", "the rested tick rises to 20 px and brightens; its neighbours rise 5 and 2 px (the comb's wave), 90 ms"],
      ["Grow", "the tick extends downward 16 px into the connector (60–140); the card's top edge opens sideways from the connector's end (100–220), then the body unrolls (160–340)"],
      ["Slide", "next tick: the connector and card follow on SNAPPY; content wipes towards the past or the future (newer comes from the right)"],
      ["Data", "toml 1.1.5 vs your pin 0.8.23 (<code>releases.json</code>): 43 breaking, none of your 80 uses change; 1.1.5 → 1.1.6: no API change"],
      ["GPUI", "same <b>unfurl</b> with a point origin; the comb wave is <code>Motion::animate</code> per tick"]],
    build() {
      const st = stage(760, 460);
      st.jb.innerHTML = segs([["package", "toml"], ["module", "value"], ["enum", "Value", true]]);
      const TV = MD.toml, vers = TV.versions.map((v) => v.v), n = vers.length, pin = vers.indexOf(TV.pinned), cw = 212;
      const x = (i) => (i / (n - 1)) * cw;
      const ticks = TV.versions.map((v, i) => { const hgt = i === pin ? 20 : v.local ? 10 : 6; return `<i class="tk${v.local ? "" : " far"}${i === pin ? " pn" : ""}" data-i="${i}" style="left:${x(i).toFixed(2)}px;height:${hgt}px"></i>`; }).join("");
      st.shelf.innerHTML = `<div class="cup">${chev("s12", "transform:rotate(180deg)")}dependencies</div>
        <div class="cbook" style="padding-bottom:6px">${gem("package", 28).el.outerHTML}<div style="display:flex;flex-direction:column;gap:1px"><span class="bn">toml</span><span class="bv">${TV.pinned}</span></div></div>
        <div class="vc">${ticks}</div><div class="cfilter" style="margin-top:14px">${ico("filter", "s12")}<span>Filter</span></div>`;
      st.reader.innerHTML = `<div class="cfol"><div class="chero">${gem("enum", 60).el.outerHTML}<div><div class="nm">Value</div><div class="ld">${esc(MD.valueToml.d)}</div></div></div></div>`;
      const i5 = vers.indexOf("1.1.5"), i6 = vers.indexOf("1.1.6");
      const L = TV.lens115;
      const A = `<div class="lh"><span class="v">1.1.5</span><span class="wh">24 days ago</span></div><div class="fc"><b style="color:var(--ink0)">43</b> breaking — none of your <b>80</b> uses change</div>
        <div class="lis">${L.rows.slice(0, 3).map(([g2, k, nm]) => `<div class="cli"><span class="g ${g2 === "~" ? "chg" : g2 === "-" ? "rem" : ""}">${g2}</span>${kind(k)}<span>${esc(nm)}</span></div>`).join("")}</div><div class="andm">and ${L.added + L.changed + L.removed + L.deprecated - 3} more</div>`;
      const B = `<div class="lh"><span class="v">1.1.6</span><span class="wh">16 days ago</span></div><div class="fc">no change to the API since 1.1.5</div><div class="andm">none of your <span style="font-style:normal">80</span> uses change</div>`;
      const W = 300;
      const cd = card(`<div class="lay la" style="display:flex;flex-direction:column;gap:9px">${A}</div><div class="lay lb" style="position:absolute;left:16px;right:16px;top:14px;display:flex;flex-direction:column;gap:9px">${B}</div>`, W);
      st.win.appendChild(cd);
      const conn = h(`<i style="position:absolute;width:1.5px;background:var(--ink1);transform-origin:50% 0;z-index:62"></i>`); st.win.appendChild(conn);
      const topE = h(`<i style="position:absolute;height:1.5px;background:var(--peri);z-index:62"></i>`); st.win.appendChild(topE);
      const ctx = { root: st.root, st, cd, conn, topE, W, i5, i6, pin };
      ctx.after = () => {
        ctx.tk = $$(st.shelf, ".vc .tk");
        ctx.b5 = box(ctx.tk[i5], st.win); ctx.b6 = box(ctx.tk[i6], st.win);
        const la = $(cd, ".la"), lb = $(cd, ".lb");
        ctx.hA = $(cd, ".ccard").getBoundingClientRect().height;
        lb.style.position = "static"; la.style.display = "none"; ctx.hB = $(cd, ".ccard").getBoundingClientRect().height; la.style.display = "flex"; lb.style.position = "absolute";
        ctx.la = la; ctx.lb = lb; ctx.base = ctx.b5.y + ctx.b5.h;
      };
      return ctx;
    },
    frame(c, t, o) {
      const R = o.reduced;
      const hovI = t < 700 ? c.i5 : c.i6;
      const px = R ? (t < 700 ? c.b5.x : c.b6.x) : spring(c.b5.x, [[700, c.b6.x, S.snappy]], S.snappy)(t);
      // the comb's wave around the rested tick
      const wave = R ? 1 : E(t, 0, 90);
      if (!c.h0) c.h0 = c.tk.map((tk) => tk.style.height);
      c.tk.forEach((tk, i) => {
        if (i === c.pin) return;
        const d = Math.abs(i - hovI);
        const lift = (d === 0 ? 10 : d === 1 ? 5 : d === 2 ? 2 : 0) * wave;
        tk.style.height = `${parseFloat(c.h0[i]) + lift}px`;
        tk.style.background = d === 0 ? "var(--ink0)" : "";
        tk.style.opacity = d === 0 ? 1 : "";
      });
      const cn = R ? 1 : E(t, 60, 80), op = R ? 1 : E(t, 100, 120), un = R ? 1 : E(t, 160, 180);
      css(c.conn, { left: `${px}px`, top: `${c.base}px`, height: "16px", transform: `scaleY(${cn.toFixed(3)})` });
      const cardX = clamp(px - c.W / 2, 8, 760 - c.W - 8), cy = c.base + 16;
      const ex = lerp(px, cardX, op), ew = lerp(0, c.W, op);
      css(c.topE, { left: `${ex}px`, top: `${cy}px`, width: `${ew}px`, opacity: op > 0.001 ? 1 : 0 });
      c.cd.style.left = `${cardX}px`; c.cd.style.top = `${cy}px`;
      const H = R ? (t < 700 ? c.hA : c.hB) : spring(c.hA, [[700, c.hB, S.snappy]], S.snappy)(t);
      $(c.cd, ".ccard").style.height = `${H}px`;
      clip(c.cd, 0, -40, H * (1 - un), -40);
      c.cd.style.visibility = un > 0.001 ? "visible" : "hidden";
      const w = R ? (t < 700 ? 0 : 1) : tween(0, [[700, 1, 160, C.glide]])(t);
      wipe(c.la, c.lb, w, c.W, 1);
    },
  });

  // ================================================================== fold: the body lives between the braces
  demo({
    id: "fold", group: "Opening", title: "Fold: between the braces", relation: "fold or unfold",
    sentence: "Unfolding a body parts its braces: the ellipsis closes, the closing brace travels down to its real line as the space between them opens, and the lines are uncovered in place; folding brings the brace back up to meet its partner.",
    w: 900, h: 460, dur: 1400, film: [0, 50, 100, 150, 200, 260, 340, 900, 960, 1020, 1100, 1250], crop: [0, 40, 900, 400],
    spec: [["Unfold", "<code>…</code> closes (0–80 ms); the tag rolls into itself (0–120); the gap opens to its lines' height and the closing brace rides its bottom edge down to its own line (60–300 ms glide); lines below are pushed, never cross-faded"],
      ["Fold", "the reverse, 200 ms: the brace rides the gap up and lands beside its opening brace, then the ellipsis opens"],
      ["Reduced", "cut; the unfolded lines keep a 2 px periwinkle gutter mark for 1.2 s"],
      ["GPUI", "<code>Presence</code> slot (Axis::Vertical, room track) + the brace as a <code>shared</code> element keyed by its token (inline → own line)"]],
    build() {
      const st = stage(900, 460, { shelf: false });
      st.jb.innerHTML = segs([["package", "present"], ["module", "glyph.rs"], ["method", "as_str", true]]);
      const L = MD.source.lines, first = MD.source.first;
      const esc2 = (s) => esc(s);
      const hl = (s) => esc2(s).replace(/\b(impl|pub|const|fn|match|self|Self)\b/g, '<span class="kw">$1</span>').replace(/(&amp;'static)/g, '<span class="kw">$1</span>').replace(/(&quot;[^&]*&quot;)/g, '<span class="st">$1</span>').replace(/(\/\/\/.*)$/, '<span class="cm">$1</span>');
      const line = (no, html2, cls = "") => `<div class="sl ${cls}"><span class="age">${no}</span><span>${html2}</span></div>`;
      const sigIdx = L.findIndex((l) => l.includes("fn as_str"));
      const top = L.slice(0, sigIdx).map((l, k) => line(first + k, hl(l))).join("");
      const sig = L[sigIdx].replace(/\{\s*$/, "");
      const body = L.slice(sigIdx + 1, sigIdx + 6);
      const endBrace = L[sigIdx + 6]; // "    }"
      st.reader.innerHTML = `<div class="cfol" style="padding-top:30px"><div class="src">
        ${top}
        <div class="sl sgl"><span class="age">${first + sigIdx}</span><span>${hl(sig)}<span class="ob">{</span><span class="ell" style="display:inline-block;overflow:hidden;vertical-align:bottom;white-space:pre"> … </span><span class="cbi">}</span><span class="ftag" style="left:auto;right:auto;position:relative;margin-left:18px;display:inline-flex;overflow:hidden;vertical-align:bottom;white-space:nowrap;color:var(--ink3);font-size:11.5px">-○ 5 lines</span></span></div>
        <div class="gap" style="overflow:hidden;position:relative">${body.map((l, k) => line(first + sigIdx + 1 + k, hl(l))).join("")}<div class="sl"><span class="age">${first + sigIdx + 6}</span><span>${esc(endBrace.replace("}", ""))}<span class="cb2" style="visibility:hidden">}</span></span></div></div>
        ${line(first + sigIdx + 7, "}")}
        ${line(first + sigIdx + 9, `<span class="kw">impl</span> fmt::Display <span class="kw">for</span> RelationLabel { … }<span style="margin-left:18px;color:var(--ink3);font-size:11.5px">-○ 5 lines</span>`)}
        ${line(first + sigIdx + 15, `<span class="kw">pub const fn</span> relation_label(kind: SemanticLinkKind, direction: RelationDirection) -&gt; RelationLabel { … }`)}
      </div></div>`;
      const brace = h(`<span style="position:absolute;font:400 13px/22px var(--mono);color:var(--ink1);z-index:3">}</span>`);
      st.win.appendChild(brace);
      const ctx = { root: st.root, st, brace };
      ctx.after = () => {
        const gap = $(st.reader, ".gap"); ctx.gap = gap; ctx.gh = gap.scrollHeight;
        ctx.ell = $(st.reader, ".ell"); ctx.ellW = ctx.ell.getBoundingClientRect().width;
        ctx.tag = $(st.reader, ".ftag"); ctx.tagW = ctx.tag.getBoundingClientRect().width;
        ctx.cbi = $(st.reader, ".cbi");
        gap.style.height = "0px";
        ctx.ob = box($(st.reader, ".ob"), st.win);
        gap.style.height = `${ctx.gh}px`;
        ctx.cb2 = box($(st.reader, ".cb2"), st.win);
        gap.style.height = "0px";
      };
      return ctx;
    },
    frame(c, t, o) {
      const R = o.reduced;
      const tw = (a, b, d1, d2, c1 = C.glide, c2 = C.glide) => (R ? (t < 900 ? 1 : 0) : tween(0, [[a, 1, d1, c1], [b, 0, d2, c2]])(t));
      const ell = tw(0, 1100, 80, 80), tag = tw(0, 1080, 120, 100), gap = tw(60, 900, 240, 200);
      c.ell.style.width = `${(c.ellW * (1 - ell)).toFixed(1)}px`;
      c.tag.style.width = `${(c.tagW * (1 - tag)).toFixed(1)}px`;
      c.gap.style.height = `${(c.gh * gap).toFixed(1)}px`;
      // the closing brace rides the gap's bottom edge from beside its partner to its own line
      const from = { x: c.ob.x + c.ob.w + (c.ellW * (1 - ell)), y: c.ob.y };
      const bx = lerp(from.x, c.cb2.x, gap), by = lerp(c.ob.y, c.cb2.y, gap);
      css(c.brace, { left: `${bx}px`, top: `${by - 3}px` });
      c.cbi.style.visibility = "hidden";
      if (R) { const m = markAt(t, 0) * (t < 900 ? 1 : 0); c.gap.style.boxShadow = m > 0 ? `inset 2px 0 0 rgba(147,162,250,${m})` : ""; }
    },
  });

  // ================================================================== x-ray (⌥): marks spell their words
  const XR = [["trait", "Deserialize", "abstract · ", "what your types become"], ["trait", "Visitor", "abstract · ", "walks one value"], ["function", "from_str", "", "parse from a string"], ["struct", "IgnoredAny", "derived · ", "skips any value"]];
  demo({
    id: "xray", group: "Opening", title: "X-ray: marks spell their words", relation: "⌥ (the same things, one rung up)",
    sentence: "Holding ⌥ makes every mark spell itself in place: the words unroll out of the mark's side in reading order, pushing what follows; letting go rolls them back into their marks all at once. A mark and its word are one thing at two widths.",
    w: 760, h: 360, dur: 1200, film: [0, 30, 60, 90, 120, 160, 220, 800, 830, 860, 900, 1000], crop: [0, 50, 760, 280],
    spec: [["In", "each word's slot widens from 0 (the row makes room) while a left-to-right clip uncovers it: 140 ms glide, rows 20 ms apart in reading order (whole screen ≤ 120 ms)"],
      ["Out", "all at once, 90 ms, accelerate: the words roll back into their marks"],
      ["Same motion", "shelf → spine (a narrow window) is x-ray in reverse: names roll into their marks"],
      ["Reduced", "cut; nothing else"],
      ["GPUI", "<code>Presence</code> with <code>Axis::Horizontal</code>; missing: a <b>clip act</b> (Pose with a reveal) so the word is uncovered, not faded"]],
    build() {
      const st = stage(760, 360, { shelf: false });
      st.jb.innerHTML = segs([["package", "serde_core"], ["module", "de", true]]);
      st.reader.innerHTML = `<div class="cfol" style="padding-top:34px;gap:6px">${XR.map(([k, n, ab, say]) => `<div class="crow2" style="grid-template-columns:18px max-content 1fr">${kind(k)}<span class="nm">${esc(n)} <span class="xw"><span class="ab">${esc(ab)}</span><span class="sy2">${esc(say)}</span></span></span><span></span></div>`).join("")}
        <div style="margin-top:22px" class="lnote"><kbd class="kbd" style="font:500 11px var(--mono);box-shadow:inset 0 0 0 1px var(--line2);padding:2px 6px;color:var(--ink2)">⌥</kbd> <span class="held" style="font:italic 400 13px var(--serif);color:var(--ink3)">held</span></div></div>`;
      const ctx = { root: st.root, st };
      ctx.after = () => { ctx.ws = $$(st.reader, ".xw").map((x) => ({ el: x, w: x.getBoundingClientRect().width })); };
      return ctx;
    },
    frame(c, t, o) {
      c.ws.forEach((x, k) => {
        const p = o.reduced ? (t < 800 ? 1 : 0) : tween(0, [[20 * k, 1, 140, C.glide], [800, 0, 90, C.drop]])(t);
        x.el.style.width = `${(x.w * p).toFixed(1)}px`;
        x.el.style.clipPath = `inset(-4px ${(x.w * (1 - p)).toFixed(1)}px -4px 0)`;
      });
      $(c.st.reader, ".kbd").style.background = t < 800 ? "rgba(147,162,250,.25)" : "";
      $(c.st.reader, ".held").textContent = t < 800 ? "held" : "released";
    },
  });

  // ================================================================== find: typing re-ranks
  demo({
    id: "find", group: "Lists", title: "Find: the list narrows around what you type", relation: "results re-rank",
    sentence: "Each key re-ranks: rows travel to their new places and leave a periwinkle wake as long as their jump, so the motion shows what the new letter changed; rows that stop matching close, new ones open at their rank, the matched letters' underline grows with your typing, and the preview rolls only when the top result changes.",
    w: 1000, h: 560, dur: 1100, film: [0, 40, 90, 130, 170, 220, 260, 300, 390, 440, 520, 580, 700], crop: [100, 50, 800, 380],
    spec: [["Travel", "every surviving row springs to its rank (SNAPPY, no stagger: typing must never lag); a key during travel retargets from where the row is"],
      ["Wake", "a 1.5 px periwinkle line from where the row was to where it is; its tail follows on a slower spring (response .5 s), so the wake's length is the rank change and it shrinks to nothing as the row settles"],
      ["Close / open", "narrowing first: a row that stops matching keeps its place while its height closes (100 ms, accelerate); a new row opens at its rank 60 ms later (160 ms) rising 4 px"],
      ["Letters", "the new character drops 4 px into its slot (90 ms); the caret follows (TRACK spring); the matched-letters underline springs to its new width"],
      ["Count / preview", "the count rolls as a count (ones first); the preview is a reel of top results, so it moves only when the top result changes"],
      ["Data", "real world names ranked by <code>build.py</code> for <code>fro → from_str</code>"],
      ["GPUI", "<code>Flow</code> (FLIP) + <code>Presence</code>; missing: the <b>wake</b> (a trailing-spring line per keyed item)"]],
    build() {
      const st = stage(1000, 560);
      st.jb.innerHTML = segs([["package", "present"], ["module", "glyph"], ["enum", "RelationLabel", true]]);
      st.shelf.innerHTML = window.PAGES.shelfHTML(true, "RelationLabel");
      st.reader.innerHTML = `<div class="cfol" style="opacity:.28"><div class="chero">${gem("enum", 60).el.outerHTML}<div><div class="nm">RelationLabel</div><div class="ld">${esc(G.RelationLabel.d)}</div></div></div><div class="ccode">${window.PAGES.code.RelationLabel}</div></div>`;
      const states = MD.search.slice(2);
      const ids = []; states.forEach((s) => s.rows.forEach((r) => { if (!ids.includes(r.id)) ids.push(r.id); }));
      const rowOf = (id) => { for (const s of states) { const r = s.rows.find((x) => x.id === id); if (r) return r; } };
      const tops = []; states.forEach((s) => { if (tops[tops.length - 1] !== s.rows[0].id) tops.push(s.rows[0].id); });
      const pv = (r) => `<div class="pvin"><div style="display:flex;align-items:center;gap:10px">${gem(r.k, 26).el.outerHTML}<div><div class="nm">${esc(r.parent ? r.parent + "::" : "")}${esc(r.n)}</div><div class="wh">${esc(r.k)} in ${esc(r.where)}</div></div></div><div class="sg">${esc(r.s || "")}</div><div class="sy">${esc(r.d || "")}</div></div>`;
      const ask = h(`<div class="afind" style="left:110px;top:66px;width:780px;height:330px">
        <div class="aq">${ico("search", "s18")}<span class="qt" style="position:relative;display:inline-flex"></span><span class="crt"></span><span class="qc"></span></div>
        <div class="rlist" style="position:absolute;left:0;top:54px;width:450px;bottom:0;overflow:hidden"><i class="selb" style="top:8px"></i>
          ${ids.map((id) => { const r = rowOf(id); return `<div class="rr" data-id="${esc(id)}" style="top:8px">${kind(r.k)}<span class="nmw" style="position:relative">${r.parent ? `<span class="par">${esc(r.parent)}::</span>` : ""}<span class="nn">${esc(r.n)}</span><i class="hitu"></i></span><span class="rwh">${esc(r.where)}</span></div>`; }).join("")}
          ${ids.map((id) => `<i class="wake" data-id="${esc(id)}" style="position:absolute;left:10px;width:1.5px;background:var(--peri);opacity:.7"></i>`).join("")}</div>
        <div class="pv" style="left:450px;right:0"><div class="pvreel" style="position:relative">${tops.map((id) => `<div class="pvp" style="position:absolute;left:0;right:0;top:0">${pv(rowOf(id))}</div>`).join("")}</div></div></div>`);
      st.win.appendChild(ask);
      const typed = "from_str".slice(3);
      $(ask, ".qt").innerHTML = `<span>fro</span>${[...typed].map((ch) => `<span class="ch" style="display:inline-block;overflow:hidden;vertical-align:bottom">${esc(ch)}</span>`).join("")}`;
      const count = odo().states(states.map((s) => String(s.count)), "count"); $(ask, ".qc").appendChild(count.el); $(ask, ".qc").insertAdjacentText("beforeend", " results");
      const ctx = { root: st.root, st, ask, states, ids, tops, count, keysAt: [0, 130, 260, 390, 520] };
      ctx.after = () => {
        const probe = $(ask, ".nn"); const r = probe.getBoundingClientRect(); ctx.cw = r.width / probe.textContent.length;
        ctx.chs = $$(ask, ".qt .ch"); ctx.chW = ctx.chs.map((c2) => c2.getBoundingClientRect().width);
        ctx.qtX = box($(ask, ".qt"), ask).x;
      };
      return ctx;
    },
    frame(c, t, o) {
      const R = o.reduced, K = c.keysAt;
      const stateAt = (tt) => { let s = 0; K.forEach((k, i) => { if (tt >= k) s = i + 1; }); return s; };
      const cur = stateAt(t);
      const RH = 36;
      // presence per row (open when it starts matching, close when it stops)
      const presOf = (id) => {
        const ranks = c.states.map((st2) => st2.rows.findIndex((r) => r.id === id));
        const first = ranks.findIndex((r) => r >= 0);
        const ev = [];
        // narrowing first: leavers close in 100 ms; arrivals open 60 ms later, so the two never share a slot
        ranks.forEach((r, k) => { if (!k) return; const on = r >= 0 ? 1 : 0, prev = ranks[k - 1] >= 0 ? 1 : 0; if (on !== prev) ev.push([K[k - 1] + (on ? 60 : 0), on, on ? 160 : 100, on ? C.glide : C.drop]); });
        return { ranks, first, p: R ? (tt) => (ranks[stateAt(tt)] >= 0 ? 1 : 0) : tween(first === 0 ? 1 : 0, ev) };
      };
      if (!c.pres) c.pres = Object.fromEntries(c.ids.map((id) => [id, presOf(id)]));
      // Flow, simulated on a 2 ms grid exactly as facet::motion::flow does it: layout is the flow of the rows'
      // current heights in rank order (a leaving row keeps its place while it closes); an epoch (a key) that
      // moves a row's layout is absorbed into its spring, so painted position never jumps
      const orderAt = (k) => {
        const cur2 = c.states[k].rows.map((r) => r.id);
        if (!k) return cur2;
        const prev = c.states[k - 1].rows.map((r) => r.id);
        const out = [...cur2];
        prev.forEach((id, i) => { if (!cur2.includes(id)) out.splice(Math.min(i, out.length), 0, id); });
        return out;
      };
      const flow = (k, tt) => { const ys = {}; let y = 8; for (const id of orderAt(k)) { ys[id] = y; y += RH * clamp(c.pres[id].p(tt)); } return ys; };
      const sim = {}; c.ids.forEach((id) => { sim[id] = { off: 0, v: 0, tail: null, tv: 0 }; });
      let k = 0, ys = flow(0, 0);
      const TAIL = { response: 0.5, damping: 1 };
      for (let tt = 0; tt <= t; tt += 2) {
        const k2 = stateAt(tt);
        if (k2 !== k) {
          const before = flow(k, tt), after = flow(k2, tt);
          c.ids.forEach((id) => { if (before[id] !== undefined && after[id] !== undefined) sim[id].off += before[id] - after[id]; });
          k = k2;
        }
        ys = flow(k, tt);
        c.ids.forEach((id) => {
          const s2 = sim[id]; if (ys[id] === undefined) return;
          if (!R) { const [x, v] = window.MO.sstep(S.snappy, s2.off, s2.v, 0.002); s2.off = x; s2.v = v; } else s2.off = 0;
          const painted = ys[id] + s2.off;
          if (s2.tail === null) s2.tail = painted;
          const [x2, v2] = window.MO.sstep(TAIL, s2.tail - painted, s2.tv, 0.002); s2.tail = painted + x2; s2.tv = v2;
        });
      }
      c.ids.forEach((id) => {
        const el = $(c.ask, `.rr[data-id="${CSS.escape(id)}"]`), wk = $(c.ask, `.wake[data-id="${CSS.escape(id)}"]`);
        const { ranks, first, p: pf } = c.pres[id];
        const p = clamp(pf(t));
        const s2 = sim[id];
        const y = ys[id] !== undefined ? ys[id] + s2.off : 8;
        el.style.top = `${y.toFixed(2)}px`;
        clip(el, 0, 0, RH * (1 - p), 0);
        tf(el, 0, ranks[cur] >= 0 ? 4 * (1 - p) : 0);
        el.style.visibility = ys[id] !== undefined && p > 0.001 ? "visible" : "hidden";
        // the wake: from where it was to where it is; its length is the jump
        const a = Math.min(y, s2.tail ?? y) + RH / 2, b = Math.max(y, s2.tail ?? y) + RH / 2;
        css(wk, { top: `${a}px`, height: `${Math.max(0, b - a - 2)}px`, display: b - a > 3 && p > 0.5 && !R ? "block" : "none" });
        // matched letters
        const r = c.states[Math.min(cur, c.states.length - 1)].rows.find((x) => x.id === id) || c.states.map((st2) => st2.rows.find((x) => x.id === id)).find(Boolean);
        const hits = r.hit || [];
        const par = $(el, ".par"); const off = par ? par.getBoundingClientRect().width : 0;
        const wEvs = []; c.states.forEach((st2, kk) => { const rr = st2.rows.find((x) => x.id === id); if (rr && kk) wEvs.push([K[kk - 1], rr.hit.length * c.cw, S.follow]); });
        const firstR = c.states[first].rows.find((x) => x.id === id);
        const uw = R ? hits.length * c.cw : spring(firstR.hit.length * c.cw, wEvs, S.follow)(t);
        css($(el, ".hitu"), { left: `${off + (hits[0] || 0) * c.cw}px`, width: `${uw}px`, bottom: "-2px" });
      });
      // typed characters drop into their slots; the caret follows
      c.chs.forEach((ch, k) => {
        const p = R ? (t >= c.keysAt[k] ? 1 : 0) : E(t, c.keysAt[k], 90);
        ch.style.width = `${(c.chW[k] * (t >= c.keysAt[k] ? 1 : 0)).toFixed(1)}px`;
        tf(ch.firstChild ? ch : ch, 0, 0);
        ch.style.transform = `translateY(${(-4 * (1 - p)).toFixed(2)}px)`;
        ch.style.visibility = t >= c.keysAt[k] ? "visible" : "hidden";
      });
      c.count.set(R ? cur : clamp(spring(0, c.keysAt.map((k, i) => [k, i + 1, S.reel]), S.reel)(t), 0, c.states.length - 1));
      // the preview is a reel of top results
      const topAt = c.states.map((s) => c.tops.indexOf(s.rows[0].id));
      const pvEvs = []; topAt.forEach((ti, k) => { if (k && ti !== topAt[k - 1]) pvEvs.push([c.keysAt[k - 1], ti, S.reel]); });
      const pv = R ? topAt[cur] : spring(topAt[0], pvEvs, S.reel)(t);
      $$(c.ask, ".pvp").forEach((pp, k) => { pp.style.transform = `translateY(${((k - pv) * 300).toFixed(1)}px)`; });
    },
  });

  // ================================================================== add: it lands in your tree
  demo({
    id: "add", group: "Lists", title: "Add a package: it lands, and brings a friend", relation: "add a package (play)",
    sentence: "Adding csv lifts its stone off the card and drops it into the list where it sorts; the list makes room just before it lands, it squashes and settles, and csv-core, the one crate it brings that your tree lacks, pops out beneath it, while the three it needs that you already have light where they already are.",
    w: 1000, h: 560, dur: 1400, film: [0, 80, 160, 240, 320, 400, 470, 540, 640, 760, 900, 1250], crop: [240, 50, 760, 510],
    spec: [["Lift", "0–140 ms: the stone lifts 8 px and grows to 1.12 (D-Browse's lift); the card rolls up into its anchor (160–320)"],
      ["Arc", "140–520 ms: a throw (quadratic, control point at the source's height 36 px up, a quarter of the way back from the slot), smoothstep progress: it leaves sideways and falls into its slot; the stone turns one quarter (the Turn primitive), never a free rotation; it grows to 1.2 at the apex and shrinks to its row-mark size"],
      ["Room", "from 280 ms: the list opens at the slot the package sorts into (BOUNCY spring, ≈ 9 % past), so the stone lands in a gap"],
      ["Land", "520–840 ms: squash and stretch (<code>keys::DROP_IN</code>'s tail: 1.25/.72 → .92/1.1 → 1.05/.95 → 1); the name unrolls from the mark (560–760)"],
      ["Friend", "760–1000 ms: the crate it brings pops out beneath it (room opens, <code>act::POP</code>); crates you already have glint in place (colour, 200 ms): they do not move, because they were already there"],
      ["Count", "the tree count rolls as a count, ones first (700–1100)"],
      ["Reduced", "cut; the new rows hold a mint mark for 1.2 s"],
      ["GPUI", "<code>Presence</code> (<code>act::DROP_IN</code>, <code>act::POP</code>) + <code>keys</code>; missing: an <b>arc flight</b> (two-axis path over one progress) and <b>Gem::turn</b>"]],
    build() {
      const st = stage(1000, 560);
      const A = MD.add;
      st.jb.innerHTML = segs([["package", "backend-facet", true]]);
      st.shelf.innerHTML = `<div class="cup">${chev("s12", "transform:rotate(180deg)")}backend</div>` + `<div class="crows">${shelfRows(["backend-cli", "backend-desktop", "backend-facet", "backend-locald", "backend-mcp", "backend-worker"].map((n) => ({ k: "package", n, cur: n === "backend-facet" })))}</div>`;
      const deps = [...A.list].sort();
      st.reader.innerHTML = `<div class="cfol" style="gap:20px;padding-top:36px">
        <div class="chero">${gem("package", 56).el.outerHTML}<div><div class="nm" style="font-size:38px">backend-facet</div><div class="ld">the Nudox design system in GPUI</div></div></div>
        <nav class="ctabs"><span>Map</span><span>Readme</span><span class="on">Depends</span><span>Used by</span><span>Changes</span></nav>
        <div class="lnote"><span class="cnt1"></span> direct · <span class="cnt2"></span> in your tree</div>
        <div class="deps" style="position:relative">${["csv", ...deps].map((n) => `<div class="crow2 dep" data-n="${n}" style="grid-template-columns:18px max-content 1fr">${n === "csv" ? `<span class="mk" style="display:inline-flex;width:18px;height:18px;align-items:center;justify-content:center"></span>` : kind("package")}<span class="nm"><span class="dn">${esc(n)}</span>${n === "csv" ? ` <span style="color:var(--ink3);font-weight:400">1.4.0</span>` : ""}</span><span></span></div>${n === "csv" ? `<div class="crow2 friend" style="grid-template-columns:18px max-content 1fr;padding-left:30px"><span class="fk" style="display:inline-flex">${gem("package", 16).el.outerHTML}</span><span class="nm" style="color:var(--ink1)">csv-core <span style="color:var(--ink3);font-weight:400">0.1.13 · new to your tree</span></span><span></span></div>` : ""}`).join("")}</div>
        <div class="lnote brings">adds 1 crate: <span class="nmk">csv-core</span> · <span class="nmk have">itoa</span>, <span class="nmk have">ryu</span>, <span class="nmk have">serde_core</span> already in your tree</div></div>`;
      const cd = card(`<div class="ch"><span class="srcg"></span><div><div class="nm">csv</div><div class="wh">1.4.0 · crates.io</div></div></div><p class="sy">${esc(A.say)}</p><div class="fc">adds <b>1</b> crate · ${esc(A.license)} fits</div><div><span class="addb" style="display:inline-flex;align-items:center;height:28px;padding:0 12px;background:var(--mint);color:var(--mint-ink);font:600 12px var(--ui)">add to backend-facet</span></div>`, 300);
      st.win.appendChild(cd); cd.style.left = "640px"; cd.style.top = "250px";
      const srcGem = gem("package", 30); $(cd, ".srcg").appendChild(srcGem.el);
      const fly = gem("package", 30); fly.el.style.position = "absolute"; fly.el.style.zIndex = 80; fly.el.style.left = "0"; fly.el.style.top = "0"; fly.el.style.color = "var(--mint)"; st.win.appendChild(fly.el);
      const n1 = odo().states([String(deps.length), String(deps.length + 1)], "count"), n2 = odo().states(["1237", "1239"], "count");
      $(st.reader, ".cnt1").appendChild(n1.el); $(st.reader, ".cnt2").appendChild(n2.el);
      const ctx = { root: st.root, st, cd, srcGem, fly, n1, n2 };
      ctx.after = () => {
        ctx.src = box(srcGem.el, st.win);
        const csvRow = $(st.reader, '.dep[data-n="csv"]'); ctx.csvRow = csvRow; ctx.rowH = csvRow.getBoundingClientRect().height;
        ctx.friend = $(st.reader, ".friend"); ctx.fH = ctx.friend.getBoundingClientRect().height;
        ctx.dst = box($(csvRow, ".mk"), st.win);
        ctx.name = $(csvRow, ".nm"); ctx.nameW = ctx.name.getBoundingClientRect().width;
        ctx.brings = $(st.reader, ".brings"); ctx.bW = ctx.brings.getBoundingClientRect().width;
        ctx.cardH = $(cd, ".ccard").getBoundingClientRect().height;
        ctx.mk = gem("package", 20); ctx.mk.el.style.color = "var(--mint)"; $(csvRow, ".mk").appendChild(ctx.mk.el);
      };
      return ctx;
    },
    frame(c, t, o) {
      const R = o.reduced;
      // the card: the button presses, then the card rolls up into its anchor
      const press = R ? 0 : P(t, 0, 60) * (1 - P(t, 80, 80));
      $(c.cd, ".addb").style.transform = `translateY(${press}px)`;
      const up = R ? (t > 0 ? 1 : 0) : E(t, 160, 160, C.drop);
      clip(c.cd, 0, -40, c.cardH * up, -40); c.cd.style.visibility = up < 0.999 ? "visible" : "hidden";
      c.srcGem.el.style.visibility = t > 0 && !R ? "hidden" : "visible";
      // the flight: lift, arc, land
      const lift = E(t, 0, 140), u = C.linear(P(t, 140, 380)), sm = u * u * (3 - 2 * u);
      const s0 = { x: c.src.x + c.src.w / 2, y: c.src.y + c.src.h / 2 - 8 * lift }, s1 = { x: c.dst.x + c.dst.w / 2, y: c.dst.y + c.dst.h / 2 };
      // a throw: it leaves the card sideways at the card's height and falls into its slot
      const cx = s1.x + (s0.x - s1.x) * 0.25, cy = Math.min(s0.y, s1.y) - 36;
      const fx = (1 - sm) ** 2 * s0.x + 2 * (1 - sm) * sm * cx + sm ** 2 * s1.x, fy = (1 - sm) ** 2 * s0.y + 2 * (1 - sm) * sm * cy + sm ** 2 * s1.y;
      const scl = u <= 0 ? 1 + 0.12 * lift : u < 0.5 ? lerp(1.12, 1.2, u / 0.5) : lerp(1.2, 18 / 30, (u - 0.5) / 0.5);
      const flying = !R && t > 0 && t < 520;
      c.fly.el.style.display = flying ? "block" : "none";
      tf(c.fly.el, fx - 15, fy - 15, scl, scl);
      c.fly.light({ turn: 3 * sm });
      // the list makes room at csv's slot, just before the landing
      const room = R ? (t > 0 ? 1 : 0) : spring(0, [[280, 1, S.bouncy]], S.bouncy)(t);
      c.csvRow.style.height = `${(c.rowH * Math.max(0, room)).toFixed(1)}px`; c.csvRow.style.minHeight = "0"; c.csvRow.style.paddingTop = c.csvRow.style.paddingBottom = `${(7 * clamp(room)).toFixed(1)}px`;
      c.csvRow.style.overflow = "hidden";
      // the landing: squash and stretch on the row's own mark
      const land = keys([[0, { sx: 1.25, sy: 0.72 }], [0.35, { sx: 0.92, sy: 1.1 }], [0.7, { sx: 1.05, sy: 0.95 }], [1, { sx: 1, sy: 1 }]], C.glide)(P(t, 520, 320));
      c.mk.el.style.visibility = R || t >= 520 ? "visible" : "hidden";
      if (!R) { c.mk.el.style.transformOrigin = "50% 100%"; tf(c.mk.el, 0, 0, land.sx, land.sy); }
      const nm = R ? (t > 0 ? 1 : 0) : E(t, 560, 200);
      c.name.style.display = "inline-block"; c.name.style.overflow = "hidden"; c.name.style.verticalAlign = "bottom"; c.name.style.width = `${(c.nameW * nm).toFixed(1)}px`;
      // the friend pops out beneath it
      const fr = R ? (t > 0 ? 1 : 0) : E(t, 760, 200);
      c.friend.style.height = `${(c.fH * fr).toFixed(1)}px`; c.friend.style.minHeight = "0"; c.friend.style.overflow = "hidden"; c.friend.style.paddingTop = c.friend.style.paddingBottom = `${(7 * fr).toFixed(1)}px`;
      const pop = keys([[0, 0.6], [0.7, 1.08], [1, 1]], C.spring)(P(t, 800, 240));
      tf($(c.friend, ".fk"), 0, 0, R ? 1 : pop, R ? 1 : pop);
      // what it brings, in words; what you already have glints where it is
      const br = R ? (t > 0 ? 1 : 0) : E(t, 900, 200);
      c.brings.style.maxWidth = `${(c.bW * br).toFixed(1)}px`; c.brings.style.overflow = "hidden";
      const gl = R ? 0 : Math.sin(Math.PI * P(t, 1050, 220));
      $$(c.st.reader, ".have").forEach((e) => { e.style.color = mixc("#9aa6ba", "#6cebad", gl); });
      c.n1.set(R ? (t > 0 ? 1 : 0) : E(t, 700, 300)); c.n2.set(R ? (t > 0 ? 1 : 0) : E(t, 760, 340));
      if (R) { const m = markAt(t, 0); [c.csvRow, c.friend].forEach((e) => { e.style.boxShadow = m > 0 ? `inset 2px 0 0 rgba(108,235,173,${m})` : ""; }); }
    },
  });

  // ================================================================== copy: the line drops into the clipboard
  demo({
    id: "copy", group: "Taking", title: "Copy: it goes where it now lives", relation: "copy (play)",
    sentence: "The install line drops through the slot under it, because that is where the clipboard is: the ghost lifts, falls through the card's bottom edge, the edge flashes mint as it passes, the ecosystem stone turns one step, and the copy glyph rolls to a tick and back.",
    w: 760, h: 400, dur: 1500, film: [0, 50, 100, 160, 220, 280, 340, 420, 600, 1100, 1180, 1300], crop: [0, 50, 760, 330],
    spec: [["Rule", "a thing you take travels to where it is kept: to the clipboard slot right under it (an install line, a path), or on an arc to the hand at the window's foot (a signature, a pin): the end of the motion is the truth about where it went"],
      ["Drop (D-Marks)", "ghost lifts 3 px (0–60 ms), falls 34 px through the slot, clipped by the slot's edge (60–360, accelerate); the slot's edge flashes mint as it passes (200–400, colour); the source text dims to ink3 and comes back"],
      ["Tick", "the ecosystem stone turns one symmetry step (the Turn primitive, 300 ms spring); the copy glyph and a tick are a two-row reel (160 ms), back after 900 ms"],
      ["Adjusted", "D-Marks' drop is 560 ms; here 360 ms. Its card unfurl scales Y from 0.04 with a 1.56 overshoot: replaced by the peek unfurl (clip), since text must never squash"],
      ["GPUI", "<code>Offset</code> + content mask; the tick is <b>Roll</b>; the stone is <b>Gem::turn</b>"]],
    build() {
      const st = stage(760, 400, { shelf: false });
      st.jb.innerHTML = segs([["package", "toml", true]]);
      st.reader.innerHTML = `<div class="cfol" style="padding-top:36px;gap:18px"><div class="chero">${gem("package", 56).el.outerHTML}<div><div class="nm" style="font-size:38px">toml</div><div class="ld">A native Rust encoder and decoder of TOML-formatted files and streams.</div></div></div>
        <div class="cfacts" style="margin-top:0;gap:14px"><span class="eco" style="display:inline-flex;align-items:center;gap:6px;color:var(--ink1)"><span class="es"></span>crates.io</span><span class="mono">0.8.23</span><span>MIT OR Apache-2.0</span></div></div>`;
      const es = gem("package", 18); $(st.reader, ".es").appendChild(es.el);
      const cd = card(`<div class="ch"><span class="cs"></span><div><div class="nm" style="font:600 13.5px var(--ui)">Rust crate · crates.io</div></div></div>
        <div class="slotw" style="position:relative"><div class="sg lineb" style="display:flex;justify-content:space-between;align-items:center"><span class="txt">cargo add toml</span><span class="cp"></span></div><i class="slote" style="position:absolute;left:0;right:0;bottom:-6px;height:1px;background:var(--line2)"></i>
        <div class="ghost" style="position:absolute;left:10px;top:7px;font:400 12px/18px var(--mono);color:var(--ink0)">cargo add toml</div></div>`, 300);
      st.win.appendChild(cd);
      const tickReel = reel([`<svg class="ico s14" viewBox="0 0 24 24">${MD.icons["ui:copy"]}</svg>`, `<svg class="ico s14" viewBox="0 0 24 24" style="color:var(--mint)"><path d="M5 12.5l4.5 4.5L19 7.5" fill="none" stroke="currentColor" stroke-width="2"/></svg>`]);
      $(cd, ".cp").appendChild(tickReel.el);
      const ctx = { root: st.root, st, cd, es, tickReel };
      ctx.after = () => {
        const b = box($(st.reader, ".eco"), st.win); cd.style.left = `${b.x - 14}px`; cd.style.top = `${b.y + b.h + 10}px`;
        const cs = gem("package", 24); $(cd, ".cs").appendChild(cs.el); ctx.cs = cs;
        ctx.ghost = $(cd, ".ghost"); ctx.slotw = $(cd, ".slotw"); ctx.slotH = ctx.slotw.getBoundingClientRect().height;
        const u = h(`<i style="position:absolute;height:1.5px;background:var(--peri);opacity:.55;left:${b.x}px;top:${b.y + b.h}px;width:${b.w}px"></i>`); st.win.appendChild(u);
      };
      return ctx;
    },
    frame(c, t, o) {
      const R = o.reduced;
      const lift = E(t, 0, 60), fall = C.drop(P(t, 60, 300));
      const gy = R ? 0 : -3 * lift * (1 - P(t, 60, 40)) + 34 * fall;
      c.ghost.style.transform = `translateY(${gy.toFixed(2)}px)`;
      // the ghost is only drawn above the slot's edge: it falls through it
      c.ghost.style.clipPath = `inset(-6px -4px ${Math.max(0, 7 + gy + 18 - (c.slotH + 6)).toFixed(1)}px -4px)`;
      c.ghost.style.visibility = !R && t > 0 && t < 420 ? "visible" : "hidden";
      $(c.cd, ".txt").style.color = !R && t > 0 && t < 420 ? mixc("#d2d9e5", "#74819a", Math.min(1, P(t, 0, 60)) * (1 - P(t, 340, 80))) : "";
      const flash = R ? 0 : Math.sin(Math.PI * P(t, 200, 200));
      $(c.cd, ".slote").style.background = mixc("#1c2a45", "#6cebad", flash);
      const turn = R ? 0 : 3 * clamp(spring(0, [[60, 1, S.snappy]], S.snappy)(t));
      c.es.light({ turn }); c.cs.light({ turn });
      const tick = R ? (t > 0 && t < 1100 ? 1 : 0) : spring(0, [[0, 1, S.reel], [1100, 0, S.reel]], S.reel)(t);
      c.tickReel.set(clamp(tick));
      if (R) { const m = markAt(t, 0); $(c.cd, ".lineb").style.boxShadow = m > 0 ? `inset 0 -1.5px 0 rgba(108,235,173,${m})` : ""; }
    },
  });

  // ================================================================== the hand: what you hold connects
  demo({
    id: "hand", group: "Taking", title: "In hand: held things connect", relation: "take (cmd-click) and connect",
    sentence: "⌘-clicking Value takes it: its name leaves the title on an arc and lands in the hand at the window's foot beside from_str, which you already hold; then the hand draws the join between them and names the verb, because from_str gives any T you choose and Value is one.",
    w: 1000, h: 560, dur: 1300, film: [0, 60, 120, 200, 280, 360, 420, 480, 560, 660, 800, 1100], crop: [0, 50, 1000, 510],
    spec: [["Take (D-Hand)", "the first card lands in the hand from where you held it: the name morphs (shared) from the page into a card on an arc (380 ms: x glide, y accelerating) and settles with a small bounce (BOUNCY on y)"],
      ["Room", "cards already in the hand move aside only if the order by what feeds what puts the new one between them (FLIP)"],
      ["Join", "420–600 ms: a periwinkle hairline draws from the giver to the taker (160 ms), the verb unrolls at its middle (<code>as</code>), then the recipe line under the hand unrolls (560–760)"],
      ["Reduced", "the card appears in place; the join is drawn at once and holds a mark for 1.2 s"],
      ["GPUI", "<b>arc flight</b> (missing) + <code>shared</code>; the join line is a painted rule with a <b>draw progress</b> (missing)"]],
    build() {
      const st = stage(1000, 560);
      st.jb.innerHTML = segs([["package", "toml"], ["module", "value"], ["enum", "Value", true]]);
      st.shelf.innerHTML = `<div class="cup">${chev("s12", "transform:rotate(180deg)")}dependencies</div><div class="crows">${shelfRows([{ k: "module", n: "value" }, { k: "type", n: "Array", depth: 1 }, { k: "enum", n: "Value", depth: 1, cur: 1 }, { k: "trait", n: "Index", depth: 1 }])}</div>`;
      st.reader.innerHTML = `<div class="cfol"><div class="chero">${gem("enum", 60).el.outerHTML}<div><div class="nm"><span class="vt">Value</span></div><div class="ld">${esc(MD.valueToml.d)}</div></div></div>
        <div class="cfacts"><span>enum in <span class="mono">toml::value</span></span><i class="sep">·</i><span>used in 59 places</span></div></div>`;
      const hand = h(`<div class="hand" style="left:${240 + 24}px;bottom:66px"><span class="hcard c1">${kind("function")}from_str</span><span class="hcard c2">${kind("enum")}Value</span></div>`);
      st.win.appendChild(hand);
      const link = h(`<i class="hlink"></i>`), verb = h(`<span class="hsay vb" style="font:italic 400 12px var(--serif);color:var(--peri-hi);background:var(--g1);padding:0 4px">as</span>`), say = h(`<span class="hsay rec"><i>from text to</i> <b>Value</b> · <b>toml::from_str::&lt;Value&gt;(text)?</b></span>`);
      st.win.appendChild(link); st.win.appendChild(verb); st.win.appendChild(say);
      const flyer = h(`<span class="hcard" style="position:absolute;left:0;top:0;z-index:80;transform-origin:0 0">${kind("enum")}Value</span>`); st.win.appendChild(flyer);
      const ctx = { root: st.root, st, hand, link, verb, say, flyer };
      ctx.after = () => {
        ctx.c1 = box($(hand, ".c1"), st.win); ctx.c2 = box($(hand, ".c2"), st.win); ctx.title = box($(st.reader, ".vt"), st.win);
        $(hand, ".c1").style.marginRight = "40px"; ctx.c2 = box($(hand, ".c2"), st.win);
        ctx.fw = flyer.getBoundingClientRect().width; ctx.fh = flyer.getBoundingClientRect().height;
        ctx.sayW = say.getBoundingClientRect().width; ctx.verbW = verb.getBoundingClientRect().width;
      };
      return ctx;
    },
    frame(c, t, o) {
      const R = o.reduced;
      // the flight: from the title's box to the card's slot on an arc
      const u = P(t, 0, 380), gx = C.glide(u), gy = C.drop(u);
      const fromS = c.title.h / c.fh; // starts at the title's size
      const x = lerp(c.title.x, c.c2.x, gx), y = lerp(c.title.y, c.c2.y, gy) - 60 * Math.sin(Math.PI * u) * 0;
      const bounce = R ? 0 : spring(0, [[380, 0, S.bouncy]], S.bouncy)(t);
      const sc = lerp(Math.min(2.2, fromS), 1, C.glide(u));
      const landing = !R && t >= 0 && t < 380;
      tf(c.flyer, x, y + bounce, sc, sc);
      c.flyer.style.display = landing ? "inline-flex" : "none";
      const c2 = $(c.hand, ".c2");
      c2.style.visibility = R ? (t >= 0 ? "visible" : "hidden") : t >= 380 ? "visible" : "hidden";
      if (!R) { const b = t >= 380 ? spring(-10, [[380, 0, S.bouncy]], S.bouncy)(t) : 0; c2.style.transform = `translateY(${b.toFixed(2)}px)`; }
      // the join
      const j = R ? (t >= 0 ? 1 : 0) : E(t, 420, 160);
      const x0 = c.c1.x + c.c1.w, x1 = c.c2.x, ym = c.c1.y + c.c1.h / 2;
      css(c.link, { left: `${x0 + 3}px`, top: `${ym}px`, width: `${(x1 - x0 - 6)}px`, transform: `scaleX(${j.toFixed(3)})` });
      const vb = R ? 1 : E(t, 500, 120);
      css(c.verb, { left: `${(x0 + x1) / 2 - c.verbW / 2}px`, top: `${ym - 9}px`, clipPath: `inset(-4px ${(c.verbW * (1 - vb)).toFixed(1)}px -4px 0)` });
      const sy = R ? 1 : E(t, 560, 200);
      css(c.say, { left: `${c.c1.x}px`, top: `${c.c1.y + c.c1.h + 10}px`, clipPath: `inset(-4px ${(c.sayW * (1 - sy)).toFixed(1)}px -4px 0)` });
      if (R) { const m = markAt(t, 0); c.link.style.boxShadow = m > 0 ? `0 0 0 1px rgba(147,162,250,${0.5 * m})` : ""; }
    },
  });
})();
