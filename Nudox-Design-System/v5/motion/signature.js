// W-Motion's signature-move board (PLAN.md storyboards). A pure function of
// ?demo=<open|close|fold|unfold|peek>&t=<ms>[&reduced=1]: every element's pose is
// computed from t alone, so a headless still is the exact frame.
(() => {
  const Q = new URLSearchParams(location.search);
  const DEMO = Q.get("demo") || "open";
  const T = +(Q.get("t") || 0);
  const REDUCED = Q.get("reduced") === "1";
  const stage = document.getElementById("stage");

  // ------------------------------------------------------------------ maths (as facet::motion)
  function bezier(p1x, p1y, p2x, p2y) {
    const cx = 3 * p1x, bx = 3 * (p2x - p1x) - cx, ax = 1 - cx - bx;
    const cy = 3 * p1y, by = 3 * (p2y - p1y) - cy, ay = 1 - cy - by;
    const sx = (t) => ((ax * t + bx) * t + cx) * t;
    const sy = (t) => ((ay * t + by) * t + cy) * t;
    const dx = (t) => (3 * ax * t + 2 * bx) * t + cx;
    return (x) => {
      if (x <= 0) return 0;
      if (x >= 1) return 1;
      let t = x;
      for (let i = 0; i < 8; i++) { const e = sx(t) - x; const d = dx(t); if (Math.abs(e) < 1e-6 || Math.abs(d) < 1e-6) break; t -= e / d; }
      return sy(t);
    };
  }
  const GLIDE = bezier(0.22, 1, 0.36, 1);
  const DROP = bezier(0.5, 0, 0.9, 0.6);
  const clamp01 = (v) => Math.max(0, Math.min(1, v));
  // CARRY: the one driver of a place change, a critically damped spring (response 0.28 s):
  // 23 % at 40 ms, 54 % at 80, 75 % at 120, 87 % at 160, 97 % at 240. Interruptible with its
  // velocity, like every facet spring.
  const carry = (ms) => { if (ms <= 0) return 0; const w = (2 * Math.PI) / 0.28, s = ms / 1000; return 1 - (1 + w * s) * Math.exp(-w * s); };
  const band = (t, a, b) => clamp01((t - a) / (b - a));
  const lerp = (a, b, p) => a + (b - a) * p;
  const px = (v) => `${v}px`;

  // ------------------------------------------------------------------ DOM
  function box(parent, cls, css) {
    const d = document.createElement("div");
    d.className = cls;
    Object.assign(d.style, css);
    parent.appendChild(d);
    return d;
  }
  const FONT = {
    mono: { cls: "mono", size: 14 },
    sans: { cls: "", size: 14 },
    serif: { cls: "serif", size: 16 },
    display: { cls: "display", size: 38 },
  };
  // A text line: [text, x, y, font, size, colour, weight]
  function text(parent, [t, x, y, font = "sans", size, colour = "var(--ink1)", weight = 400], extra = {}) {
    const f = FONT[font];
    const s = size ?? f.size;
    const d = box(parent, `el ${f.cls}`, { left: px(x), top: px(y), fontSize: px(s), lineHeight: px(Math.round(s * 1.3)), color: colour, fontWeight: weight, ...extra });
    d.textContent = t;
    return d;
  }
  function gem(parent, x, y, size, css = {}) {
    const g = box(parent, "gem", { left: px(x), top: px(y), width: px(size), height: px(size), ...css });
    const d = box(g, "", { inset: "14%" });
    d.style.position = "absolute";
    d.style.transform = "rotate(45deg)";
    d.style.border = `${Math.max(1.5, size / 26)}px solid var(--enum)`;
    d.style.background = "rgba(108,209,176,.08)";
    box(g, "", { position: "absolute", left: "38%", top: "38%", width: "24%", height: "24%", background: "var(--enum)" });
    return g;
  }
  function clip(parent, x, y, w, h) {
    return box(parent, "clip", { left: px(x), top: px(y), width: px(Math.max(0, w)), height: px(Math.max(0, h)) });
  }

  // ------------------------------------------------------------------ the pages (read off the app at 1440 x 900)
  const INK = { 0: "var(--ink0)", 1: "var(--ink1)", 2: "var(--ink2)", 3: "var(--ink3)" };
  const PAGE_A = {
    title: "RelationLabel",
    lines: [
      ["The readable label of one relation group.", 532, 134, "serif", 20, INK[1]],
      ["enum", 478, 174, "sans", 13, INK[3]], ["present::glyph", 535, 174, "sans", 13, INK[3]], ["glyph.rs:138", 638, 174, "sans", 13, INK[3]],
      ["RelationLabel has 3 variants. Explore ways to make one, operations, connections, and author notes.", 460, 211, "sans", 15, INK[1]],
      ["Reference", 460, 251, "sans", 14, INK[0], 500], ["Relations", 541, 251, "sans", 14, INK[2]], ["Usage", 617, 251, "sans", 14, INK[2]], ["History", 675, 251, "sans", 14, INK[2]], ["Source", 738, 251, "sans", 14, INK[2]],
      ["one of", 460, 296, "sans", 12, INK[2]],
      ["Typed", 482, 327, "mono", 14, INK[0], 700], ["SemanticLinkKind", 541, 327, "mono", 14, INK[1], 400, "clicked"], ["×", 674, 327, "mono", 13, INK[3]], ["RelationDirection", 689, 327, "mono", 14, INK[1]],
      ["A relation whose compiler kind and direction are both known.", 840, 325, "serif", 15, INK[2]],
      ["Neighbourhood", 482, 361, "mono", 14, INK[0], 700], ["A bounded neighbourhood whose per-edge kind the reply did not carry.", 606, 359, "serif", 15, INK[2]],
      ["Related", 482, 395, "mono", 14, INK[0], 700], ["Incoming and outgoing neighbours whose per-edge kind is not carried.", 557, 393, "serif", 15, INK[2]],
      ["Getting one", 460, 460, "sans", 17, INK[0], 650, "h:getting"],
      ["SemanticLinkKind", 482, 491, "mono", 14, INK[1], 400, "in:0"], ["relation_label", 634, 491, "mono", 14, INK[0], 700], ["RelationLabel", 873, 491, "mono", 14, INK[1]],
      ["RelationDirection", 640, 517, "mono", 14, INK[1], 400, "in:1"], ["direction", 784, 517, "mono", 14, INK[1]],
      ["comes from", 499, 542, "sans", 13, INK[3]], ["relation_label", 594, 542, "mono", 14, INK[1]], ["RelationGroup::label", 726, 542, "mono", 14, INK[1], 400, "in:2"],
      ["What it does", 460, 610, "sans", 17, INK[0], 650, "h:does"],
      ["The usual 6 capabilities", 478, 656, "sans", 13, INK[1]],
      ["Inspect", 482, 706, "sans", 13, INK[2]],
      ["as_str", 491, 743, "mono", 14, INK[0], 700, "out:0"], ["text", 589, 743, "mono", 14, INK[2], 400, "out:2"],
      ["Returns the words this group prints.", 485, 769, "serif", 15, INK[2]],
      ["through its traits", 460, 814, "sans", 12, INK[2]],
      ["Display", 490, 846, "mono", 14, INK[1], 400, "out:1"], ["fmt", 568, 846, "mono", 14, INK[1]],
    ],
  };
  const PAGE_B = {
    title: "SemanticLinkKind",
    lines: [
      ["The closed vocabulary of how one symbol touches another.", 532, 134, "serif", 20, INK[1]],
      ["enum", 478, 174, "sans", 13, INK[3]], ["present::relation", 535, 174, "sans", 13, INK[3]], ["relation.rs:41", 663, 174, "sans", 13, INK[3]],
      ["Reference", 460, 221, "sans", 14, INK[0], 500], ["Relations", 541, 221, "sans", 14, INK[2]], ["Usage", 617, 221, "sans", 14, INK[2]], ["History", 675, 221, "sans", 14, INK[2]], ["Source", 738, 221, "sans", 14, INK[2]],
      ["one of", 460, 266, "sans", 12, INK[2]],
      ["Calls", 482, 296, "mono", 14, INK[0], 700], ["A direct call to a free function or associated item.", 560, 294, "serif", 15, INK[2]],
      ["MethodCall", 482, 330, "mono", 14, INK[0], 700], ["A call through a receiver, resolved by the compiler.", 598, 328, "serif", 15, INK[2]],
      ["Implements", 482, 364, "mono", 14, INK[0], 700], ["A type implementing a trait.", 598, 362, "serif", 15, INK[2]],
      ["Extends", 482, 398, "mono", 14, INK[0], 700], ["A trait bound on a supertrait.", 572, 396, "serif", 15, INK[2]],
      ["Uses", 482, 432, "mono", 14, INK[0], 700], ["Any other named reference.", 540, 430, "serif", 15, INK[2]],
      ["Getting one", 460, 488, "sans", 17, INK[0], 650],
      ["SemanticLinkKind::from_str", 482, 519, "mono", 14, INK[0], 700], ["text", 712, 519, "mono", 14, INK[2]],
      ["comes from", 499, 545, "sans", 13, INK[3]], ["RelationLabel::kind", 594, 545, "mono", 14, INK[1]],
      ["What it does", 460, 602, "sans", 17, INK[0], 650],
      ["as_str", 491, 640, "mono", 14, INK[0], 700], ["text", 589, 640, "mono", 14, INK[2]],
      ["The word this kind prints.", 485, 666, "serif", 15, INK[2]],
      ["is_call", 491, 710, "mono", 14, INK[0], 700], ["bool", 600, 710, "mono", 14, INK[2]],
      ["Whether the link runs code.", 485, 736, "serif", 15, INK[2]],
      ["Who uses it", 460, 796, "sans", 17, INK[0], 650],
      ["RelationLabel", 482, 832, "mono", 14, INK[1]], ["relation_label", 612, 832, "mono", 14, INK[1]], ["typed_label", 742, 832, "mono", 14, INK[1]],
    ],
  };
  const TITLE = { x: 530, y: 88, size: 38 };
  const HERO_GEM = { x: 460, y: 100, size: 52 };
  const CLICKED = { x: 541, y: 327, size: 14, w: 126 };

  // ------------------------------------------------------------------ chrome (titlebar + shelf), shared by every demo
  const SHELF_ROWS = ["assemble", "browse", "budget", "call", "coverage", "drive", "dto", "fault", "glyph", "DeclarationKind", "SemanticLinkKind", "fmt", "Language", "KindGlyph", "LanguageGlyph", "RelationDirection", "RelationLabel", "relation_label", "typed_label", "grammar", "identity", "language", "lib", "outline"];
  function chrome(root, { bar = null, jump = null } = {}) {
    box(root, "", { position: "absolute", left: 0, top: 0, width: "1440px", height: "50px", background: "var(--g1)", borderBottom: "1px solid var(--line1)" });
    box(root, "", { position: "absolute", left: 0, top: "50px", width: "264px", height: "850px", background: "var(--g1)", borderRight: "1px solid var(--line1)" });
    text(root, ["Library", 26, 60, "sans", 13, INK[2]]);
    text(root, ["present", 45, 84, "sans", 15, INK[0], 650]);
    text(root, ["0.1.0", 45, 104, "mono", 11, INK[2]]);
    box(root, "", { position: "absolute", left: "10px", top: "130px", width: "244px", height: "28px", border: "1px solid var(--line2)", background: "var(--g0)" });
    text(root, ["Filter", 32, 136, "sans", 13, INK[3]]);
    if (bar != null) {
      box(root, "", { position: "absolute", left: 0, top: px(bar - 4), width: "264px", height: "30px", background: "var(--peri-soft)" });
      box(root, "", { position: "absolute", left: 0, top: px(bar - 4), width: "3px", height: "30px", background: "var(--mint)" });
    }
    SHELF_ROWS.forEach((name, i) => {
      const nested = i >= 9 && i <= 18;
      text(root, [name, nested ? 43 : 31, 172 + i * 30, "mono", 13.5, INK[1]]);
    });
    // The jump bar: present › <segment> › <here>, a reel per segment.
    box(root, "", { position: "absolute", left: "680px", top: "9px", width: "304px", height: "31px", background: "var(--g2)", border: "1px solid var(--line2)" });
    text(root, ["present", 693, 16, "sans", 13, INK[2]]);
    text(root, ["›", 741, 15, "sans", 13, INK[3]]);
    const reel = (x, w, from, to, p, weight) => {
      const c = clip(root, x, 14, w, 20);
      text(c, [from, 0, 1 - 20 * p, "sans", 13, weight ? INK[0] : INK[2], weight ? 650 : 400]);
      if (to) text(c, [to, 0, 1 + 20 * (1 - p), "sans", 13, weight ? INK[0] : INK[2], weight ? 650 : 400]);
    };
    const j = jump ?? { seg: ["glyph", null], here: ["RelationLabel", null], p: 0 };
    reel(752, 60, j.seg[0], j.seg[1], j.p, false);
    text(root, ["›", 806, 15, "sans", 13, INK[3]]);
    reel(822, 150, j.here[0], j.here[1], j.p, true);
    if (j.mark) box(root, "", { position: "absolute", left: "818px", top: "12px", width: "154px", height: "24px", border: `1px solid rgba(147,162,250,${j.mark})`, background: `rgba(147,162,250,${0.13 * j.mark})` });
  }

  // Draws a page (hero + lines) into `parent`, with per-line hooks.
  function page(parent, spec, { title = true, gemAt = null, line = null } = {}) {
    if (gemAt !== false) gem(parent, HERO_GEM.x, HERO_GEM.y, HERO_GEM.size, gemAt ?? {});
    if (title) text(parent, [spec.title, TITLE.x, TITLE.y, "display", TITLE.size, INK[0]]);
    spec.lines.forEach((l, i) => {
      if (line) line(l, i);
      else text(parent, l);
    });
  }

  // The periwinkle "you are here" mark of reduced motion: 1.2 s hold, 160 ms settle.
  const mark = (t) => (t < 0 ? 0 : t < 1200 ? 1 : 1 - clamp01((t - 1200) / 160));

  // ================================================================== OPEN: the row opens into the page
  // t=0 is the click frame. The clicked row's plate is the new page's ground: its top edge
  // rises to the reader's top carrying the name (reset in the title face on frame one), its
  // bottom edge falls to the reader's floor, covering the old page as it goes (nothing fades,
  // nothing is under anything). The old page drifts 16 px left (in, to the right); the new page
  // prints beneath the landed title in reading order, line by line.
  function open(t) {
    const cut = REDUCED ? (t >= 0 ? 1 : 0) : null;
    const p = carry(t);
    const up = cut ?? band(p, 0, 0.9);
    const down = cut ?? band(p, 0, 0.94);
    const top = lerp(CLICKED.y - 8, 50, up);
    const bottom = lerp(CLICKED.y + 26, 900, down);
    const left = lerp(452, 264, up);
    const right = lerp(1248, 1440, up);
    const drift = cut != null ? 0 : -16 * p;
    const toward = cut != null ? 0 : 16 * (1 - band(p, 0.5, 1));
    const shelf = lerp(172 + 16 * 30, 172 + 10 * 30, cut ?? band(p, 0.1, 0.9));
    const roll = cut ?? band(p, 0.3, 0.8);
    chrome(stage, { bar: shelf, jump: { seg: ["glyph", "relation"], here: ["RelationLabel", "SemanticLinkKind"], p: roll, mark: REDUCED ? mark(t) : 0 } });

    // The old page, drifting left; the clicked token leaves it on frame one.
    const reader = clip(stage, 264, 50, 1176, 850);
    const old = box(reader, "layer", { transform: `translateX(${drift}px)`, left: "-264px", top: "-50px" });
    if (t < 0) {
      // At rest: the pointer is on the token (the hover grammar has lit it).
      box(old, "", { position: "absolute", left: "452px", top: px(CLICKED.y - 8), width: "796px", height: "34px", background: "var(--plate)", border: "1px solid var(--line2)" });
    }
    page(old, PAGE_A, {
      line: (l) => {
        if (l[7] === "clicked" && t >= 0) return;
        text(old, l, l[7] === "clicked" ? { color: "var(--ink0)", borderBottom: "1px solid var(--mint)" } : {});
      },
    });
    if (t < 0) return;

    // The plate: the row's ground, opening. It is the new page's ground.
    const plate = box(stage, "", { position: "absolute", left: px(left), top: px(top), width: px(right - left), height: px(bottom - top), background: "var(--g1)", borderTop: up < 1 ? "1px solid var(--peri-line)" : "none", boxShadow: up < 1 ? "0 -12px 24px rgba(0,0,0,.35)" : "none" });
    plate.style.overflow = "hidden";
    const inner = box(plate, "layer", { left: px(-left + toward), top: px(-top) });

    // The name becomes the title: it rides just below the rising edge, grows, and lands.
    const g = cut ?? band(p, 0.02, 0.92);
    const size = lerp(CLICKED.size, TITLE.size, g);
    const y = Math.max(TITLE.y, lerp(CLICKED.y, TITLE.y, g), top + 6 - (size - CLICKED.size) * 0.2);
    const x = lerp(CLICKED.x, TITLE.x, g);
    text(stage, [PAGE_B.title, x, Math.max(TITLE.y, Math.min(y, top + 8)), "display", size, INK[0]]);

    // The gem unfurls from the title's leading edge once the title is near its slot.
    const gp = cut ?? band(p, 0.55, 0.95);
    if (gp > 0) gem(inner, HERO_GEM.x, HERO_GEM.y, HERO_GEM.size, { transform: `scale(${gp})`, transformOrigin: "100% 50%" });

    // The page prints beneath the title, line by line in reading order (by y).
    const order = PAGE_B.lines.map((l, i) => [l[2], i]).sort((a, b) => a[0] - b[0]);
    const rank = new Map(order.map(([, i], k) => [i, k]));
    const rows = [...new Set(order.map(([y]) => y))];
    PAGE_B.lines.forEach((l, i) => {
      const row = rows.indexOf(l[2]);
      const start = 100 + row * 8;
      const p = cut ?? band(t, start, start + 32);
      if (p <= 0) return;
      const h = Math.round(l[4] * 1.3) + 4;
      const c = clip(inner, l[1], l[2] - 2, 900, h * p);
      text(c, [l[0], 0, 2 + (1 - p) * 8, l[3], l[4], l[5], l[6]]);
      void rank;
    });
    return { top, bottom };
  }

  // ================================================================== CLOSE (Back): the page closes into its row
  function close(t) {
    const cut = REDUCED ? (t >= 0 ? 1 : 0) : null;
    // The body folds first, last lines first, all of it gone before the title moves.
    const rollUp = (i, n) => cut != null ? cut : GLIDE(band(t, (n - 1 - i) * 1.5, (n - 1 - i) * 1.5 + 36));
    const q = carry(t - 64);
    const shut = cut ?? band(q, 0, 0.9);
    const top = lerp(50, CLICKED.y - 8, shut);
    const bottom = lerp(900, CLICKED.y + 26, shut);
    const left = lerp(264, 452, shut);
    const right = lerp(1440, 1248, shut);
    const back = cut != null ? 0 : -16 * (1 - GLIDE(band(t, 40, 240)));
    const shelf = lerp(172 + 10 * 30, 172 + 16 * 30, cut ?? GLIDE(band(t, 30, 190)));
    chrome(stage, { bar: shelf, jump: { seg: ["relation", "glyph"], here: ["SemanticLinkKind", "RelationLabel"], p: cut ?? GLIDE(band(t, 60, 180)) } });
    const reader = clip(stage, 264, 50, 1176, 850);
    const parent = box(reader, "layer", { transform: `translateX(${back}px)`, left: "-264px", top: "-50px" });
    // The row you came back to keeps a periwinkle tint: where you were.
    const were = cut != null ? mark(t) : t < 180 ? 0 : t < 700 ? 1 : 1 - clamp01((t - 700) / 240);
    if (were > 0 || shut >= 1) box(parent, "", { position: "absolute", left: "452px", top: px(CLICKED.y - 8), width: "796px", height: "34px", background: `rgba(147,162,250,${0.09 * were})` });
    page(parent, PAGE_A, {
      line: (l) => {
        if (l[7] === "clicked" && shut < 1) return;
        text(parent, l);
      },
    });
    if (shut >= 1) return;
    const plate = box(stage, "", { position: "absolute", left: px(left), top: px(top), width: px(right - left), height: px(bottom - top), background: "var(--g1)", borderTop: shut > 0 ? "1px solid var(--peri-line)" : "none", overflow: "hidden" });
    const inner = box(plate, "layer", { left: px(-left), top: px(-top) });
    const order = [...new Set(PAGE_B.lines.map((l) => l[2]))].sort((a, b) => a - b);
    PAGE_B.lines.forEach((l) => {
      const p = 1 - rollUp(order.indexOf(l[2]), order.length);
      if (p <= 0) return;
      const h = Math.round(l[4] * 1.3) + 4;
      const c = clip(inner, l[1], l[2] - 2, 900, h * p);
      text(c, [l[0], 0, 2, l[3], l[4], l[5], l[6]]);
    });
    const gp = 1 - (cut ?? GLIDE(band(t, 0, 90)));
    if (gp > 0) gem(inner, HERO_GEM.x, HERO_GEM.y, HERO_GEM.size, { transform: `scale(${gp})`, transformOrigin: "100% 50%" });
    // The title shrinks back into its row, riding the closing edge down.
    const g = cut ?? band(q, 0.05, 0.95);
    const size = lerp(TITLE.size, CLICKED.size, g);
    const y = Math.max(lerp(TITLE.y, CLICKED.y, g), top + 6);
    text(stage, [PAGE_B.title, lerp(TITLE.x, CLICKED.x, g), Math.min(y, CLICKED.y), "display", size, INK[0]]);
  }

  // ================================================================== FOLD: the page closes into its node
  const NODE = { x: 720, y: 450, size: 30 };
  const SLOTS = {
    "in:0": { x: 620, y: 392, name: "SemanticLinkKind", side: -1 },
    "in:1": { x: 620, y: 442, name: "RelationDirection", side: -1 },
    "in:2": { x: 620, y: 492, name: "RelationGroup::label", side: -1 },
    "out:0": { x: 820, y: 392, name: "as_str", side: 1 },
    "out:1": { x: 820, y: 442, name: "Display", side: 1 },
    "out:2": { x: 820, y: 492, name: "text", side: 1 },
  };
  function rng(seed) { let s = seed >>> 0; return () => ((s = (s * 1664525 + 1013904223) >>> 0) / 4294967296); }
  const WORLD = (() => {
    const r = rng(7);
    const nodes = [];
    for (let i = 0; i < 70; i++) {
      const a = r() * Math.PI * 2, d = 120 + Math.pow(r(), 0.8) * 620;
      nodes.push({ x: NODE.x + Math.cos(a) * d * 1.35, y: NODE.y + Math.sin(a) * d * 0.8 });
    }
    const edges = [];
    for (let i = 0; i < nodes.length; i++) { const j = Math.floor(r() * nodes.length); if (j !== i) edges.push([nodes[i], nodes[j]]); }
    return { nodes, edges };
  })();

  function graph(root, zoom, strokes, labels, nodeMark) {
    box(root, "layer", { background: "var(--g0)" });
    const g = box(root, "layer", { transformOrigin: `${NODE.x}px ${NODE.y}px`, transform: `scale(${zoom})`, overflow: "visible" });
    const svg = document.createElementNS("http://www.w3.org/2000/svg", "svg");
    svg.setAttribute("class", "edges");
    svg.setAttribute("viewBox", "0 0 1440 900");
    for (const [a, b] of WORLD.edges) {
      const l = document.createElementNS("http://www.w3.org/2000/svg", "line");
      l.setAttribute("x1", a.x); l.setAttribute("y1", a.y); l.setAttribute("x2", b.x); l.setAttribute("y2", b.y);
      l.setAttribute("stroke", "rgba(158,176,255,.07)"); l.setAttribute("stroke-width", 1 / zoom);
      svg.appendChild(l);
    }
    for (const [key, s] of Object.entries(SLOTS)) {
      const p = strokes[key] ?? 0;
      if (p <= 0) continue;
      const sx = NODE.x + (s.side * NODE.size) / 2, sy = NODE.y;
      const ex = s.x + (s.side < 0 ? 14 : -14), ey = s.y + 8;
      const l = document.createElementNS("http://www.w3.org/2000/svg", "path");
      const mx = lerp(sx, ex, 0.5);
      l.setAttribute("d", `M${sx},${sy} C${mx},${sy} ${mx},${ey} ${ex},${ey}`);
      l.setAttribute("stroke", s.side < 0 ? "rgba(147,162,250,.75)" : "rgba(108,235,173,.75)");
      l.setAttribute("stroke-width", 1.5 / zoom); l.setAttribute("fill", "none");
      l.setAttribute("pathLength", 1); l.setAttribute("stroke-dasharray", `${p} 1`);
      svg.appendChild(l);
    }
    g.appendChild(svg);
    for (const n of WORLD.nodes) box(g, "dot", { left: px(n.x - 2.5), top: px(n.y - 2.5) });
    for (const [key, s] of Object.entries(SLOTS)) {
      if (!(labels[key] >= 1)) continue;
      const nx = s.x + (s.side < 0 ? 6 : -6);
      box(g, "", { position: "absolute", left: px(nx - 5), top: px(s.y + 3), width: "10px", height: "10px", transform: "rotate(45deg)", border: "1.5px solid var(--ink3)", background: "var(--g2)" });
    }
    if (nodeMark) box(g, "", { position: "absolute", left: px(NODE.x - 26), top: px(NODE.y - 26), width: "52px", height: "52px", transform: "rotate(45deg)", border: `1.5px solid rgba(147,162,250,${nodeMark})` });
    return g;
  }

  // t: 0..480. Sections fold up into their headers (0-110); the page's plate shrinks into the
  // node's plate, uncovering the graph that was always behind it (40-220); the rows that are
  // relations travel out along their own strokes to their slots, left for what it comes from,
  // right for what it goes into (60-210), and each stroke draws behind its label; the gem and
  // title become the node and its label (40-210); then the camera pulls back (200-480).
  function fold(t) {
    const cut = REDUCED ? (t >= 0 ? 1 : 0) : null;
    const shrink = cut ?? GLIDE(band(t, 40, 220));
    const out = cut != null ? (cut ? 0.62 : 1) : lerp(1, 0.62, GLIDE(band(t, 200, 480)));
    const travel = (key) => {
      const i = key.startsWith("in") ? +key.slice(3) : 3 + +key.slice(4);
      return cut ?? GLIDE(band(t, 60 + i * 10, 200 + i * 10));
    };
    const strokes = {}, labels = {};
    for (const key of Object.keys(SLOTS)) { strokes[key] = cut ?? band(t, 130 + 8 * +key.slice(-1), 220 + 8 * +key.slice(-1)); labels[key] = travel(key); }
    const root = box(stage, "layer", {});
    graph(root, out, strokes, labels, REDUCED ? mark(t) : 0);
    chrome(stage, {});
    // The page's plate, shrinking into the node's plate.
    const pl = { l: lerp(264, NODE.x - 18, shrink), t: lerp(50, NODE.y - 18, shrink), r: lerp(1440, NODE.x + 18, shrink), b: lerp(900, NODE.y + 18, shrink) };
    const moving = box(stage, "layer", { transformOrigin: `${NODE.x}px ${NODE.y}px`, transform: `scale(${out})` });
    if (shrink < 1) {
      const plate = box(moving, "", { position: "absolute", left: px(pl.l), top: px(pl.t), width: px(pl.r - pl.l), height: px(pl.b - pl.t), background: "var(--g1)", overflow: "hidden", borderTop: shrink > 0 ? "1px solid var(--peri-line)" : "none" });
      const inner = box(plate, "layer", { left: px(-pl.l), top: px(-pl.t) });
      // Sections fold: every non-relation line rolls up into its header, last rows first.
      const ys = [...new Set(PAGE_A.lines.map((l) => l[2]))].sort((a, b) => a - b);
      PAGE_A.lines.forEach((l) => {
        if (l[7] && (l[7].startsWith("in:") || l[7].startsWith("out:"))) return;
        const k = ys.indexOf(l[2]);
        const p = 1 - (cut ?? GLIDE(band(t, (ys.length - 1 - k) * 3, (ys.length - 1 - k) * 3 + 36)));
        if (p <= 0) return;
        const h = Math.round(l[4] * 1.3) + 4;
        const c = clip(inner, l[1], l[2] - 2, 900, h * p);
        text(c, [l[0], 0, 2, l[3], l[4], l[5], l[6]]);
      });
    }
    // Gem -> node, title -> node label (shared elements, uniform scale).
    const hp = cut ?? GLIDE(band(t, 40, 210));
    const gs = lerp(HERO_GEM.size, NODE.size, hp);
    gem(moving, lerp(HERO_GEM.x, NODE.x - NODE.size / 2, hp), lerp(HERO_GEM.y, NODE.y - NODE.size / 2, hp), gs);
    const ts = lerp(TITLE.size, 13, hp);
    const tw = PAGE_A.title.length * ts * 0.52;
    text(moving, [PAGE_A.title, lerp(TITLE.x, NODE.x - tw / 2, hp), lerp(TITLE.y, NODE.y + 22, hp), hp > 0.55 ? "mono" : "display", ts, INK[0], hp > 0.55 ? 500 : 700]);
    // The relation rows travel out along their strokes to their slots.
    PAGE_A.lines.forEach((l) => {
      const key = l[7];
      if (!key || !SLOTS[key]) return;
      const s = SLOTS[key];
      const p = labels[key];
      const w = s.name.length * 7.3;
      const endX = s.side < 0 ? s.x - w : s.x;
      text(moving, [s.name, lerp(l[1], endX, p), lerp(l[2], s.y, p), "mono", lerp(14, 12.5, p), INK[1]]);
    });
  }

  // ================================================================== PEEK: ink rises, then the word opens
  // t=0: the pointer rests on `SemanticLinkKind`. At once its ink rises one step, its hit shape
  // draws its bevel stroke, and every other occurrence lights. At 350 ms the peek unfurls: the
  // underline draws, becomes the card's top edge, and the body unrolls from it. Leave at 900.
  const TOKEN = { x: 541, y: 327, w: 126, h: 18 };
  const CARD = { x: 520, y: 356, w: 440, h: 170 };
  function peek(t) {
    chrome(stage, {});
    const reader = clip(stage, 264, 50, 1176, 850);
    const base = box(reader, "layer", { left: "-264px", top: "-50px" });
    const hovering = t >= 0 && t < 1210;
    // How far the card's body has unrolled (for the page lines it covers).
    const cover = (() => {
      if (t < 350) return 0;
      const u = t - 350, leave = Math.max(0, t - 900);
      return REDUCED ? (t < 900 ? 1 : 0) : GLIDE(band(u, 100, 220)) * (1 - DROP(band(leave, 0, 120)));
    })();
    page(base, PAGE_A, {
      line: (l) => {
        const lit = hovering && (l[0] === "SemanticLinkKind");
        // A line the card's body has reached steps down to the ground's ink: one thing speaks.
        const w = l[0].length * (l[3] === "mono" ? 8.4 : 7.2);
        const under = cover > 0 && l[2] + 18 > CARD.y && l[2] < CARD.y + CARD.h * cover && l[1] < CARD.x + CARD.w && l[1] + w > CARD.x;
        text(base, l, lit ? { color: "var(--ink0)" } : under ? { color: "var(--ink4)" } : {});
        if (lit && l[7] !== "clicked") box(base, "", { position: "absolute", left: px(l[1]), top: px(l[2] + 19), width: px(l[0].length * 8.4), height: "1px", background: "rgba(108,209,176,.55)" });
      },
    });
    if (hovering) {
      // The shelf's occurrence lights too, and the relation stroke to it.
      box(stage, "", { position: "absolute", left: "43px", top: px(172 + 10 * 30 + 19), width: "134px", height: "1px", background: "rgba(108,209,176,.55)" });
      box(stage, "", { position: "absolute", left: px(TOKEN.x - 6), top: px(TOKEN.y - 4), width: px(TOKEN.w + 12), height: px(TOKEN.h + 8), border: "1px solid rgba(108,209,176,.5)" });
    }
    if (t < 350) return;
    const u = t - 350;
    const leave = Math.max(0, t - 900);
    const cut = REDUCED ? 1 : null;
    // Stage 1: the underline draws under the token (0-60); reverse last on leave.
    const draw = cut ?? (GLIDE(band(u, 0, 60)) * (1 - GLIDE(band(leave, 210, 300))));
    // Stage 2: the underline becomes the card's top edge (60-140); shrinks back on leave (120-210).
    const edge = cut ?? (GLIDE(band(u, 60, 140)) * (1 - GLIDE(band(leave, 120, 210))));
    // Stage 3: the body unrolls from the edge (100-220); rolls up first on leave (0-120).
    const body = cut ?? (GLIDE(band(u, 100, 220)) * (1 - DROP(band(leave, 0, 120))));
    if (REDUCED && t >= 900) return;
    const lx = lerp(TOKEN.x, CARD.x, edge), lw = lerp(TOKEN.w * draw, CARD.w, edge), ly = lerp(TOKEN.y + TOKEN.h + 2, CARD.y, edge);
    if (lw > 0) box(stage, "", { position: "absolute", left: px(lx), top: px(ly), width: px(lw), height: "1.5px", background: "var(--peri)" });
    if (body <= 0) return;
    const card = box(stage, "", { position: "absolute", left: px(CARD.x), top: px(CARD.y + 1.5), width: px(CARD.w), height: px((CARD.h - 1.5) * body), background: "var(--plate2)", borderLeft: "1px solid var(--line3)", borderRight: "1px solid var(--line3)", borderBottom: body > 0.98 ? "1px solid var(--line3)" : "none", overflow: "hidden", boxShadow: "0 18px 30px rgba(0,0,0,.45)" });
    const c = box(card, "layer", { left: "0", top: "0" });
    gem(c, 20, 18, 30);
    text(c, ["SemanticLinkKind", 64, 16, "mono", 17, INK[0], 700]);
    text(c, ["enum in present::relation", 64, 40, "sans", 13, INK[2]]);
    box(c, "", { position: "absolute", left: "20px", top: "66px", width: "400px", height: "28px", background: "var(--g1)" });
    text(c, ["pub enum SemanticLinkKind { Calls, MethodCall… }", 30, 71, "mono", 13, INK[1]]);
    text(c, ["The closed vocabulary of how one symbol touches another.", 20, 104, "serif", 16, INK[1]]);
    text(c, ["14", 20, 138, "sans", 14, "var(--mint)", 700]);
    text(c, ["uses in your code", 42, 138, "sans", 14, INK[2]]);
  }

  // ------------------------------------------------------------------ run
  const DEMOS = { open, close, fold, unfold: (t) => fold(480 - t), peek };
  const run = DEMOS[DEMO] ?? open;
  run(DEMO === "open" || DEMO === "close" || DEMO === "fold" || DEMO === "unfold" ? T : T);
  document.body.dataset.ready = "1";
})();
