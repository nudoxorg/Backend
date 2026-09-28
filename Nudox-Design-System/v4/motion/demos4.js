/* FACET v4 — demos4.js: state (progress, a fault) and the window (theme, narrow, text size, tabs, focus). */
(() => {
  "use strict";
  const { C, S, spring, tween, clamp, P, E, lerp, band, keys, mixc, tf, clip, noclip, css, box, h, esc, kind, ico, chev, gem, reel, hreel, odo, stage, segs, shelfRows, demo } = window.MO;
  const { markAt } = window.DEMO_HELPERS;
  const MD = window.MD;
  const $ = (root, s) => root.querySelector(s);
  const $$ = (root, s) => [...root.querySelectorAll(s)];
  const PG = window.PAGES, G = PG.G;
  const rgba = (s) => { if (s.startsWith("#")) { const x = s.slice(1); return [0, 2, 4].map((i) => parseInt(x.slice(i, i + 2), 16)).concat(1); } const m = s.match(/[\d.]+/g).map(Number); return m.length === 3 ? m.concat(1) : m; };
  const mixa = (a, b, p) => { const x = rgba(a), y = rgba(b); const q = clamp(p); return `rgba(${x.slice(0, 3).map((v, i) => Math.round(lerp(v, y[i], q))).join(",")},${lerp(x[3], y[3], q).toFixed(3)})`; };

  // ================================================================== progress: the stone fills as the work seals
  const WORK = [60, 120, 170, 300, 340, 380, 560, 610, 650, 700, 900, 980]; // twelve units of real work, at their real pace
  const WORDS = [[0, "fetching"], [2, "unpacking"], [4, "reading 36 files"], [9, "resolving items"], [11, "sealing"], [12, "ready"]];
  demo({
    id: "progress", group: "State", title: "Progress: the stone fills", relation: "progress",
    sentence: "Indexing lights the stone one facet per unit of real work, each facet flashing as it seals, at the work's own pace, so a slow step visibly waits; light travels the row's edge while it works, and when the twelfth facet seals one glint crosses the stone and the edge goes still.",
    w: 760, h: 300, dur: 1400, film: [0, 70, 130, 200, 320, 420, 580, 680, 800, 920, 1000, 1150], crop: [160, 76, 440, 150],
    spec: [["Facets", "facet k lights when unit k of the work seals (no smoothing, no fake progress); it flashes to full and settles to its light over 120 ms"],
      ["Working", "light travels the plate's edge (period 1.6 s, the Pulse clock) only while the work runs"],
      ["Seal", "one glint crosses the facets (200 ms), the dashed rim closes, the edge stops: finished, with no check mark and no toast"],
      ["Words", "the stage word rolls on a reel (fetching → unpacking → reading → resolving → sealing → ready)"],
      ["Reduced", "facets still light (it is information), without the flash, the glint or the travelling edge"],
      ["GPUI", "<code>Gem::progress</code> + <code>GemState::Working</code> exist; missing: a <b>flash</b> on the newest facet and a <b>one-shot glint</b> (today's glint loops)"]],
    build() {
      const st = stage(760, 300, { shelf: false });
      st.jb.innerHTML = segs([["package", "toml", true]]);
      const rows = [["toml", "1.1.6", "work"], ["serde_core", "1.0.229", "done"], ["csv", "1.4.0", "queued"]];
      st.reader.innerHTML = `<div class="cfol" style="padding-top:30px;gap:8px;max-width:520px">${rows.map(([n, v, s]) => `<div class="prow ${s}" style="position:relative;display:flex;align-items:center;gap:14px;height:52px;padding:0 16px;background:var(--plate2);box-shadow:inset 1px 1px 0 var(--bevel-hi),inset -1px -1px 0 var(--bevel-lo)"><span class="pg"></span><span style="font:600 14px var(--mono);color:var(--ink0)">${n}</span><span style="font:400 12px var(--mono);color:var(--ink3)">${v}</span><span class="pw" style="margin-left:auto;font:italic 400 13.5px var(--serif);color:var(--ink3)">${s === "done" ? "ready" : s === "queued" ? "next" : ""}</span>${s === "work" ? `<svg class="edge2" style="position:absolute;inset:0;width:100%;height:100%;overflow:visible" preserveAspectRatio="none"><rect x="0.5" y="0.5" width="99%" height="51" fill="none" stroke="var(--mint)" stroke-width="1.5"></rect></svg>` : ""}</div>`).join("")}</div>`;
      const gs = $$(st.reader, ".pg").map((el, i) => { const g = gem("package", i ? 30 : 40); el.appendChild(g.el); return g; });
      gs[1].light({}); gs[2].light({ progress: 0 }); gs[2].rim(true);
      const words = reel(WORDS.map(([, w]) => esc(w))); $(st.reader, ".work .pw").appendChild(words.el);
      return { root: st.root, st, g: gs[0], words, after() { const r = $(st.reader, ".edge2 rect"); this.rect = r; const b = r.getBBox(); this.per = 2 * (b.width + b.height); r.style.strokeDasharray = `60 ${this.per}`; } };
    },
    frame(c, t, o) {
      const R = o.reduced;
      const n = WORK.filter((w) => t >= w).length;
      const last = n ? WORK[n - 1] : -1;
      const flash = !R && n && t - last < 120 ? n - 1 + P(t, last, 120) : -1;
      const glint = !R && n === 12 ? P(t, 980, 200) : -1;
      c.g.light({ progress: n, flash, glint: glint >= 0 && glint < 1 ? glint : -1 });
      c.g.rim(n < 12);
      const wi = WORDS.filter(([k]) => n >= k).length - 1;
      const wEvs = []; WORDS.forEach(([k], i) => { if (i) wEvs.push([WORK[Math.min(k, 12) - 1] ?? 0, i, S.reel]); });
      c.words.set(R ? wi : spring(0, wEvs, S.reel)(t));
      // light travels the edge while working
      const on = n < 12 && !R;
      c.rect.style.display = on ? "block" : "none";
      c.rect.style.strokeDashoffset = `${(-(t / 1600) * c.per).toFixed(1)}`;
    },
  });

  // ================================================================== a fault: said once, in place, with the operand
  demo({
    id: "fault", group: "State", title: "A fault: where it stopped, in place", relation: "a fault (a failed load)",
    sentence: "Opening a release that cannot be fetched shows how far it got: the stone fills for each step that worked, the facet that failed turns coral and a crack is drawn into it; then the fault is said once, under the hero, and only its exact operand, the version, pulses once. Nothing shakes and nothing else moves.",
    w: 1000, h: 480, dur: 1400, film: [0, 60, 140, 260, 360, 480, 530, 580, 640, 720, 860, 1200], crop: [100, 50, 800, 330],
    spec: [["Progress", "the stone lights one facet per step that worked (fetch index, find, download…), as in Progress"],
      ["Fail", "480 ms: the next facet flashes coral, the crack draws from the rim into the table (160 ms), the stone's hue becomes coral (the cracked state)"],
      ["Say it once", "the sentence unrolls below the facts line (height + clip, 200 ms); the operand's underline pulses once (0 → 1 → .45, 400 ms) and stays at .45"],
      ["Never", "no shake (it reads as \"no\"), no red wash, no toast; the page you pinned stays as it was"],
      ["Reduced", "the crack and sentence appear at once; the operand holds its underline"],
      ["GPUI", "<code>GemState::Cracked</code> exists; missing: <b>crack draw progress</b> and the <b>pulse-once</b> track (a colour keyframe)"]],
    build() {
      const st = stage(1000, 480, { shelf: false });
      st.jb.innerHTML = segs([["package", "toml"], ["module", "value"], ["enum", "Value", true]]);
      st.reader.innerHTML = `<div class="cfol" style="gap:22px"><div class="chero"><span class="hg"></span><div><div class="nm">Value</div><div class="ld">${esc(MD.valueToml.d)}</div></div></div>
        <div class="cfacts"><span>enum in <span class="mono">toml::value</span></span><i class="sep">·</i><span>opening <span class="mono">1.1.4</span></span></div>
        <div class="fw" style="height:0;overflow:hidden;margin-top:-8px"><div class="mfault">Couldn't fetch toml <span class="op">1.1.4<i class="opu" style="position:absolute;left:0;right:0;bottom:-2px;height:1.5px;background:var(--coral)"></i></span>: crates.io did not answer, and it is not on this machine.<span class="fdo">try again</span></div></div>
        <div class="csec oneof" style="opacity:.9"><div class="cgrp" style="padding-top:0">one of · <span class="mono">0.8.23</span>, your pin</div>${MD.valueToml.kids.filter((k) => k.k === "variant").slice(0, 4).map((v) => `<div class="ov"><span>${esc(v.n)}</span><span class="ty2"></span><span class="dsc">${esc(v.d)}</span></div>`).join("")}</div></div>`;
      const g = gem("enum", 60); $(st.reader, ".hg").appendChild(g.el);
      return { root: st.root, st, g, after() { this.fh = $(st.reader, ".fw").scrollHeight; } };
    },
    frame(c, t, o) {
      const R = o.reduced;
      const steps = [40, 110, 170, 260, 330];
      const n = steps.filter((s) => t >= s).length;
      const failed = t >= 480;
      const last = n ? steps[n - 1] : -1;
      const flash = !R && !failed && n && t - last < 120 ? n - 1 + P(t, last, 120) : failed && !R ? 5 + P(t, 480, 160) : -1;
      c.g.light({ progress: failed ? 6 : n, flash, coralFrom: failed ? 5 : 99 });
      c.g.crack(R ? (failed ? 1 : 0) : E(t, 480, 160));
      c.g.color(failed ? mixc("#5fe0b4", "#ff7a8a", R ? 1 : E(t, 480, 160)) : "");
      const fw = $(c.st.reader, ".fw");
      const u = R ? (t >= 560 ? 1 : 0) : E(t, 560, 200);
      fw.style.height = `${(c.fh * u).toFixed(1)}px`;
      const pulse = R ? (t >= 560 ? 1 : 0) : keys([[0, 0], [0.4, 1], [1, 0.45]], C.glide)(P(t, 700, 400));
      $(c.st.reader, ".opu").style.opacity = pulse.toFixed(3);
    },
  });

  // ================================================================== theme: the light changes
  const PAL = {
    "--g0": ["#030814", "#dfe5ee"], "--g1": ["#060d1b", "#eef2f7"], "--ink0": ["#f5f7fb", "#0a1222"], "--ink1": ["#d2d9e5", "#1d2940"], "--ink2": ["#9aa6ba", "#4b5a75"],
    "--ink3": ["#74819a", "#66758f"], "--ink4": ["#4c5870", "#a3aec2"], "--line1": ["rgba(158,176,255,.07)", "rgba(30,50,110,.08)"], "--line2": ["rgba(158,176,255,.12)", "rgba(30,50,110,.14)"],
    "--line3": ["rgba(158,176,255,.22)", "rgba(30,50,110,.26)"], "--plate2": ["#101d33", "#f3f6fb"], "--plate3": ["#16273f", "#e9eef6"], "--glass": ["#132039", "#ffffff"],
    "--bevel-hi": ["rgba(196,210,255,.26)", "rgba(255,255,255,1)"], "--bevel-lo": ["rgba(0,0,0,.6)", "rgba(30,50,110,.22)"], "--mint": ["#6cebad", "#0f9d6a"], "--peri": ["#93a2fa", "#4b5bd6"],
    "--f-ns": ["#a9b6cc", "#55647e"], "--f-type": ["#5fe0b4", "#0b8f68"], "--f-con": ["#d59cf5", "#8b3fc0"], "--f-call": ["#8fa6ff", "#3d52d0"],
  };
  demo({
    id: "theme", group: "Window", title: "Theme: the light changes", relation: "theme (Abyss → Glacier)",
    sentence: "Switching the theme changes the light, so it arrives the way the house light falls, from the top-left: each region takes the new palette as the sweep reaches it, and the stones catch the new light with one glint as it lands on them.",
    w: 1100, h: 560, dur: 900, film: [0, 60, 120, 180, 240, 300, 360, 420, 520, 800], crop: [0, 0, 1100, 560],
    spec: [["Choice", "the selection plate slides to the new option (SNAPPY), 0–240 ms"],
      ["Sweep", "each region (titlebar, shelf, reader, status) mixes its palette roles and ground over 200 ms (glide), starting at <code>60 + 280 · (x + y) / (W + H)</code> ms from its top-left: light falling across the window; rows inside a region follow its ground 20 ms apart (ink only)"],
      ["Glint", "every stone glints once when its region lands (200 ms)"],
      ["Reduced", "every region switches at once"],
      ["GPUI", "regions are already separate entities; missing: a <b>palette mix</b> per region (a <code>Facet</code> global with a transition value each region reads with its own delay)"]],
    build() {
      const st = stage(1100, 560);
      st.ground.style.display = "none";
      st.jb.innerHTML = segs([[null, "Settings"], [null, "Appearance", true]]).replace('<span class="jseg"><span class="sn">Settings', `<span class="jseg">${ico("settings", "s14")}<span class="sn">Settings`);
      st.shelf.innerHTML = `<div class="cup">${chev("s12", "transform:rotate(180deg)")}Nudox</div><div class="cbook"><span class="ag"></span><div style="display:flex;flex-direction:column;gap:1px"><span class="bn">Nudox</span><span class="bv">settings</span></div></div>
        <div class="crows">${["Appearance", "Index & registries", "Keys", "Notifications", "About"].map((n, i) => `<div class="crow${i ? "" : " cur"}" style="font-family:var(--ui)">${ico(["eye", "server", "key", "bell", "info"][i], "s14")}<span class="n">${esc(n)}</span></div>`).join("")}</div>`;
      const seg = (opts, on) => `<div class="seg3" style="position:relative"><i class="pl"></i>${opts.map((x, i) => `<span data-i="${i}" class="${i === on ? "on" : ""}">${x}</span>`).join("")}</div>`;
      st.reader.innerHTML = `<div class="cfol" style="gap:0;max-width:640px"><h1 class="thd" style="font:700 34px/1 var(--display);letter-spacing:-.03em;color:var(--ink0);margin:0 0 20px">Appearance</h1>
        <div class="srow"><span>Theme</span>${seg(["<span class='tg0'></span>System", "<span class='tg1'></span>Abyss", "<span class='tg2'></span>Glacier"], 1)}</div>
        <div class="srow"><span>Contrast</span>${seg(["Normal", "High"], 0)}</div>
        <div class="srow"><span>Density</span>${seg(["Comfortable", "Compact", "Dense"], 0)}</div>
        <div class="srow"><span>Motion</span>${seg(["System", "Full", "Reduced"], 0)}</div></div>`;
      const gems = [];
      const ag = gem("package", 28); $(st.shelf, ".ag").appendChild(ag.el); gems.push(ag);
      ["tg0", "tg1", "tg2"].forEach((k) => { const g = gem("module", 14); $(st.reader, `.${k}`).appendChild(g.el); gems.push(g); });
      const ctx = { root: st.root, st, gems };
      ctx.after = () => {
        const W = 1100, H = 560;
        // regions own a background; rows inside the reader only change ink, a little after the reader does
        const rb = box(st.reader, st.win), rd = 60 + 280 * (rb.x + rb.y) / (W + H);
        ctx.regs = [st.tb, st.shelf, st.reader, st.status].map((el) => { const b = box(el, st.win); return { el, bg: true, d: el === st.reader ? rd : 60 + 280 * (b.x + b.y) / (W + H) }; })
          .concat([$(st.reader, ".thd"), ...$$(st.reader, ".srow")].map((el, k) => ({ el, bg: false, d: rd + 20 * k })));
        const s0 = $(st.reader, ".seg3"); const sp = $$(s0, "span[data-i]"); ctx.theme = { pl: $(s0, ".pl"), xs: sp.map((e) => box(e, s0)) };
        $$(st.reader, ".seg3").forEach((s) => { const on = $(s, "span.on"); const b = box(on, s); css($(s, ".pl"), { left: `${b.x}px`, width: `${b.w}px`, top: "0" }); });
        ctx.gemReg = [1, 5, 5, 5];
      };
      return ctx;
    },
    frame(c, t, o) {
      const R = o.reduced;
      const x = R ? (t >= 0 ? 2 : 1) : spring(1, [[0, 2, S.snappy]], S.snappy)(t);
      const i = Math.floor(x), f = x - i, a = c.theme.xs[i], b = c.theme.xs[Math.min(2, i + 1)];
      css(c.theme.pl, { left: `${lerp(a.x, b.x, f)}px`, width: `${lerp(a.w, b.w, f)}px` });
      $$(c.st.reader, ".seg3")[0].querySelectorAll("span[data-i]").forEach((s, k) => s.classList.toggle("on", k === Math.round(x)));
      const shelfBg = ["rgba(3,8,18,.55)", "rgba(255,255,255,.6)"];
      c.st.win.style.background = mixa(PAL["--g1"][0], PAL["--g1"][1], R ? 1 : E(t, c.regs[2].d, 200));
      c.regs.forEach((r, k) => {
        const p = R ? 1 : E(t, r.d, 200);
        for (const [v, [A, B]] of Object.entries(PAL)) r.el.style.setProperty(v, mixa(A, B, p));
        if (r.bg) r.el.style.background = r.el === c.st.shelf ? mixa(shelfBg[0], shelfBg[1], p) : mixa(PAL["--g1"][0], PAL["--g1"][1], p);
        r.el.style.color = "var(--ink1)";
      });
      c.gems.forEach((g, k) => {
        const reg = c.regs[c.gemReg[k]];
        const gl = R ? -1 : P(t, reg.d + 120, 200);
        g.light({ glint: gl > 0 && gl < 1 ? gl : -1 });
      });
    },
  });

  // ================================================================== narrow: the shelf becomes the spine
  demo({
    id: "narrow", group: "Window", title: "Narrow: the shelf becomes its marks", relation: "the window goes narrow (shelf → spine)",
    sentence: "Dragging the window narrower is followed directly; crossing 900 px is an epoch, and the shelf's names roll back into their marks (the same motion as letting go of ⌥) while the shelf springs to the 42 px spine and the page follows its edge.",
    w: 1100, h: 560, dur: 900, film: [0, 120, 240, 290, 320, 350, 390, 440, 520, 800], crop: [0, 0, 1100, 560],
    spec: [["Drag", "the window's width follows the pointer with no easing (Flow tracks continuous drags)"],
      ["Epoch", "at the 900 px breakpoint: names clip right → left into their marks (140 ms, rows 10 ms apart, top first); the book's name and version roll into its stone; the filter closes into its glyph; the shelf width springs 240 → 42 (GENTLE)"],
      ["Widen", "the reverse: names unroll out of their marks"],
      ["Reduced", "the spine appears at the breakpoint"],
      ["GPUI", "<code>Flow</code> epochs on <code>Room</code> exist; the roll is the x-ray <b>clip act</b> on each row"]],
    build() {
      const st = stage(1100, 560);
      st.root.style.background = "#02050c";
      st.jb.innerHTML = segs([["package", "present"], ["module", "glyph"], ["enum", "RelationLabel", true]]);
      st.shelf.innerHTML = PG.shelfHTML(true, "RelationLabel");
      st.reader.innerHTML = `<div class="cfol" style="gap:22px"><div class="chero">${gem("enum", 60).el.outerHTML}<div><div class="nm">RelationLabel</div><div class="ld">${esc(G.RelationLabel.d)}</div></div></div>${PG.symbolBody("RelationLabel")}</div>`;
      const ctx = { root: st.root, st };
      ctx.after = () => {
        ctx.names = $$(st.shelf, ".crow .n").map((n) => ({ el: n, w: n.getBoundingClientRect().width }));
        ctx.book = $(st.shelf, ".cbook>div"); ctx.bookW = ctx.book.getBoundingClientRect().width;
        ctx.rows = $$(st.shelf, ".crow");
        $$(st.shelf, ".curbar,.curbg").forEach((e) => e.remove());
        const bar = $(st.reader, ".ctabs .bar"); bar.style.width = `${$(st.reader, ".ctabs .on").getBoundingClientRect().width}px`;
      };
      return ctx;
    },
    frame(c, t, o) {
      const R = o.reduced;
      const W = lerp(1100, 800, clamp(t / 420)); // the pointer's path
      c.st.win.style.width = `${W}px`;
      const tc = (1100 - 900) / 300 * 420; // when it crosses 900
      const sw = R ? (t >= tc ? 42 : 240) : spring(240, [[tc, 42, S.gentle]], S.gentle)(t);
      c.st.shelf.style.width = `${sw.toFixed(1)}px`;
      c.names.forEach((n, k) => {
        const p = R ? (t >= tc ? 1 : 0) : E(t, tc + 10 * k, 140, C.drop);
        n.el.style.display = "inline-block"; n.el.style.textOverflow = "clip"; n.el.style.width = `${(n.w * (1 - p)).toFixed(1)}px`; n.el.style.clipPath = `inset(0 ${(n.w * p).toFixed(1)}px 0 0)`;
      });
      const bp = R ? (t >= tc ? 1 : 0) : E(t, tc, 160, C.drop);
      c.book.style.width = `${(c.bookW * (1 - bp)).toFixed(1)}px`; c.book.style.overflow = "hidden";
      const up = $(c.st.shelf, ".cup"), fl = $(c.st.shelf, ".cfilter");
      up.style.overflow = "hidden"; up.style.width = `${lerp(240, 42, bp)}px`; up.style.padding = `0 ${lerp(14, 15, bp)}px`;
      fl.style.width = `${lerp(218, 22, bp)}px`; fl.style.margin = `0 ${lerp(10, 10, bp)}px 8px`; fl.style.padding = `0 ${lerp(10, 5, bp)}px`;
      c.rows.forEach((r) => { r.style.paddingLeft = `${lerp(parseFloat(r.dataset.pl || (r.dataset.pl = parseFloat(getComputedStyle(r).paddingLeft))), 13, bp)}px`; });
      $(c.st.shelf, ".cbook").style.padding = `8px ${lerp(14, 7, bp)}px 12px`;
    },
  });

  // ================================================================== text size: everything grows around what you read
  demo({
    id: "textsize", group: "Window", title: "Text size: the line you read stays put", relation: "text size 100 → 150 %",
    sentence: "Changing the text size keeps the line you are reading under your eye: that heading grows from its own corner and stays where it was, and everything else flows to its new place around it, above growing upward and below growing downward.",
    w: 1000, h: 600, dur: 700, film: [0, 40, 80, 120, 160, 220, 300, 420, 600], crop: [0, 50, 1000, 520],
    spec: [["Anchor", "the element at the reading line (the first heading or row whose top is past 35 % of the viewport) keeps its top-left; the scroll offset is set so that holds at the new size"],
      ["Flow", "every keyed element springs from its old bounds to its new ones (GENTLE), text through the compositing layer's scale so glyphs are re-rasterized, never stretched; wraps that change land at the end"],
      ["Reduced", "the new size at once, still anchored (the anchoring is not motion, it is where you are)"],
      ["GPUI", "<code>Flow</code> with <code>Resize::Scale</code> on a text-scale epoch exists; missing: the <b>scroll anchor</b> (keep an item's painted top fixed across an epoch)"]],
    build() {
      const st = stage(1000, 600, { shelf: false });
      st.jb.innerHTML = segs([["package", "present"], ["module", "glyph"], ["enum", "RelationLabel", true]]);
      const page = (z) => `<div class="tz" style="position:absolute;left:0;right:0;top:0;zoom:${z}"><div class="cfol" style="gap:22px;padding-top:30px">
        <div class="chero" data-k="hero">${gem("enum", 60).el.outerHTML}<div><div class="nm">RelationLabel</div><div class="ld">${esc(G.RelationLabel.d)}</div></div></div>
        <div class="cfacts" data-k="facts"><span>enum in <span class="mono">present::glyph</span></span><i class="sep">·</i><span>line <span class="mono">138</span></span></div>
        <div class="ccode" data-k="code">${PG.code.RelationLabel}</div>
        <div class="csec"><h2 data-k="anchor">Made of</h2>${PG.madeOf.RelationLabel.map(([k, nm, say], i) => `<div class="crow2" data-k="r${i}">${kind(k)}<span class="nm">${nm}</span><span class="say">${esc(say)}</span></div>`).join("")}</div></div></div>`;
      st.reader.innerHTML = page(1) + page(1.5);
      const [A, B] = $$(st.reader, ".tz");
      return { root: st.root, st, A, B, after() {
        const R = st.reader;
        const bx = (root) => Object.fromEntries($$(root, "[data-k]").map((e) => { const r = e.getBoundingClientRect(), o = R.getBoundingClientRect(); return [e.dataset.k, { x: r.left - o.left, y: r.top - o.top, w: r.width, h: r.height, el: e }]; }));
        this.a = bx(A); const b0 = bx(B);
        this.dy = this.a.anchor.y - b0.anchor.y; // scroll the new layout so the anchor stays
        B.style.top = `${this.dy / 1.5}px`;
        this.b = bx(B);
        A.style.visibility = "hidden";
      } };
    },
    frame(c, t, o) {
      const p = o.reduced ? 1 : spring(0, [[0, 1, S.gentle]], S.gentle)(t);
      for (const k of Object.keys(c.b)) {
        const a = c.a[k], b = c.b[k];
        const s = lerp(a.h / b.h, 1, p);
        b.el.style.transformOrigin = "0 0";
        // transform inside a zoomed box is in unzoomed px: divide by the zoom
        b.el.style.transform = `translate(${((a.x - b.x) * (1 - p) / 1.5).toFixed(2)}px,${((a.y - b.y) * (1 - p) / 1.5).toFixed(2)}px) scale(${s.toFixed(4)})`;
      }
    },
  });

  // ================================================================== tabs: side by side
  demo({
    id: "tabs", group: "Window", title: "Tabs: side by side", relation: "sideways within a page",
    sentence: "Tabs sit side by side, so switching moves sideways: the underline travels leading edge first, and the content is wiped in the same direction, the new view coming in from where its tab is.",
    w: 1000, h: 560, dur: 700, film: [0, 40, 80, 120, 160, 200, 260, 400], crop: [240, 60, 760, 440],
    spec: [["Underline", "its leading edge moves on SNAPPY, its trailing edge on a slower spring (response .36): it stretches and gathers (inchworm), 240 ms"],
      ["Content", "wipe (two content masks), 200 ms glide, 12 px offset; the boundary travels against the movement"],
      ["Relations", "the Relations tab speaks the sentence of DIRECTIONS §4, one grammar for every language"],
      ["GPUI", "two <code>Motion</code> springs for the underline; the <b>wipe</b> swap is missing"]],
    build() {
      const st = stage(1000, 560);
      st.jb.innerHTML = segs([["package", "present"], ["module", "glyph"], ["enum", "RelationLabel", true]]);
      st.shelf.innerHTML = PG.shelfHTML(true, "RelationLabel");
      st.reader.innerHTML = `<div class="cfol" style="gap:22px"><div class="chero">${gem("enum", 60).el.outerHTML}<div><div class="nm">RelationLabel</div><div class="ld">${esc(G.RelationLabel.d)}</div></div></div>
        <nav class="ctabs"><span class="on">Reference</span><span>Relations</span><span>Usage</span><span>History</span><span>Source</span><i class="bar"></i></nav>
        <div class="tabc" style="position:relative;height:300px"><div class="ta" style="position:absolute;inset:0;display:flex;flex-direction:column;gap:22px"><div class="ccode">${PG.code.RelationLabel}</div></div>
        <div class="tb2" style="position:absolute;inset:0"><p style="font:italic 400 19px/1.55 var(--serif);color:var(--ink1);margin:0;max-width:56ch"><span style="font:600 16px var(--mono);font-style:normal;color:var(--ink0)">RelationLabel</span> <b style="font:600 17px var(--ui);color:var(--ink0);font-style:normal">is</b> an enum of three kinds, <b style="font:600 17px var(--ui);color:var(--ink0);font-style:normal">made of</b> <u>SemanticLinkKind</u> and <u>RelationDirection</u>, <b style="font:600 17px var(--ui);color:var(--ink0);font-style:normal">becomes</b> text through <u>as_str</u>, and <b style="font:600 17px var(--ui);color:var(--ink0);font-style:normal">is used</b> in 3 places in present.</p></div></div></div>`;
      return { root: st.root, st, after() { const tabs = $$(st.reader, ".ctabs span"); this.t0 = box(tabs[0], $(st.reader, ".ctabs")); this.t1 = box(tabs[1], $(st.reader, ".ctabs")); this.W = $(st.reader, ".tabc").getBoundingClientRect().width; } };
    },
    frame(c, t, o) {
      const R = o.reduced;
      const lead = R ? 1 : spring(0, [[0, 1, S.snappy]], S.snappy)(t), trail = R ? 1 : spring(0, [[0, 1, { response: 0.36, damping: 1 }]], { response: 0.36, damping: 1 })(t);
      const l = lerp(c.t0.x, c.t1.x, trail), r = lerp(c.t0.x + c.t0.w, c.t1.x + c.t1.w, lead);
      css($(c.st.reader, ".ctabs .bar"), { left: `${l}px`, width: `${r - l}px` });
      $$(c.st.reader, ".ctabs span").forEach((s, k) => s.classList.toggle("on", k === (lead > 0.5 ? 1 : 0)));
      const w = R ? 1 : E(t, 0, 200);
      const A = $(c.st.reader, ".ta"), B = $(c.st.reader, ".tb2"), W = c.W, x = W * (1 - w);
      clip(A, -10, W - x, -10, -10); clip(B, -10, -10, -10, x); tf(A, -12 * w, 0); tf(B, 12 * (1 - w), 0);
    },
  });

  // ================================================================== focus: one ring, travelling
  demo({
    id: "focus", group: "Window", title: "Focus: one ring that travels", relation: "focus moves (keyboard)",
    sentence: "Focus is one periwinkle ring that travels from the row you left to the row you reached, its leading edge first, so the eye is carried along instead of finding a ring that vanished in one place and appeared in another.",
    w: 900, h: 400, dur: 900, film: [0, 30, 60, 90, 130, 200, 230, 260, 300, 400, 460, 600], crop: [100, 60, 700, 280],
    spec: [["Travel", "leading edge on SNAPPY, trailing edge on response .36 (inchworm); a key during travel retargets both"],
      ["Never", "two rings, or a ring that fades out here and in there"],
      ["GPUI", "the ring as one <code>shared</code> element keyed <code>focus</code> per window, with two edge springs (missing: <b>edge-split bounds morph</b>)"]],
    build() {
      const st = stage(900, 400, { shelf: false });
      st.jb.innerHTML = segs([["package", "present"], ["module", "glyph"], ["enum", "RelationLabel", true]]);
      st.reader.innerHTML = `<div class="cfol" style="padding-top:34px"><div class="csec" style="position:relative"><h2>Made of</h2>${PG.rows2(PG.madeOf.RelationLabel)}${PG.rows2([["method", "as_str", "Returns the words this group prints."]])}<i class="fring" style="position:absolute;left:-10px;right:-10px;box-shadow:inset 2px 2px 0 var(--peri-hi),inset -2px -2px 0 var(--peri);background:rgba(147,162,250,.06)"></i></div></div>`;
      return { root: st.root, st, after() { const sec = $(st.reader, ".csec"); this.ys = $$(sec, ".crow2").map((r) => box(r, sec)); } };
    },
    frame(c, t, o) {
      const R = o.reduced, K = [0, 200, 440];
      const idx = (tt) => K.filter((k) => tt >= k).length;
      const lead = R ? idx(t) : spring(0, K.map((k, i) => [k, i + 1, S.snappy]), S.snappy)(t);
      const trail = R ? idx(t) : spring(0, K.map((k, i) => [k, i + 1, { response: 0.36, damping: 1 }]), { response: 0.36, damping: 1 })(t);
      const Y = (v) => { const i = clamp(Math.floor(v), 0, c.ys.length - 1), j = Math.min(i + 1, c.ys.length - 1); return { top: lerp(c.ys[i].y, c.ys[j].y, v - i), bot: lerp(c.ys[i].y + c.ys[i].h, c.ys[j].y + c.ys[j].h, v - i) }; };
      const a = Y(trail), b = Y(lead);
      css($(c.st.reader, ".fring"), { top: `${a.top}px`, height: `${b.bot - a.top}px` });
    },
  });
})();
