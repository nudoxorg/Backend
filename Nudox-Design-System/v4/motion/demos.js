/* FACET v4 — demos.js: every transition of the motion grammar, on real content (data.js, built by build.py).
 * Each demo: build(opts) -> ctx (DOM + measurements), frame(ctx, t, opts) -> styles at t ms. Pure in t. */
(() => {
  "use strict";
  const { C, S, spring, tween, clamp, P, E, lerp, band, keys, mixc, tf, clip, noclip, css, box, h, esc, kind, ico, chev, gem, reel, odo, stage, segs, shelfRows, demo } = window.MO;
  const MD = window.MD;
  const INOUT = MO.bezier(0.4, 0, 0.2, 1); // the scrub head: steady through the middle, so every release is felt
  const $ = (root, s) => root.querySelector(s);
  const $$ = (root, s) => [...root.querySelectorAll(s)];
  const MARK = 1200; // reduced motion: how long a mark holds before it settles

  /** Reduced motion: the relation is marked, not moved. A mark holds MARK ms, then settles in 160 ms (colour only). */
  const markAt = (t, t0) => (t < t0 ? 0 : t < t0 + MARK ? 1 : 1 - P(t, t0 + MARK, 160));

  /** crossing times of a driver s(t) over thresholds (sampled on a 2 ms grid; deterministic) */
  function crossings(s, thresholds, tEnd) {
    const out = thresholds.map(() => []);
    let prev = s(0);
    for (let t = 2; t <= tEnd; t += 2) {
      const v = s(t);
      thresholds.forEach((th, i) => { if ((prev < th && v >= th) || (prev >= th && v < th)) out[i].push([t, v >= th ? 1 : 0]); });
      prev = v;
    }
    return out;
  }
  const beatTrack = (evs, dur, curve = C.glide, init = 0) => tween(init, evs.map(([t, on]) => [t, on, dur, curve]));

  // ================================================================== 1. version scrub: the history, played
  const TV = MD.toml, facts = TV.facts, vers = facts.map((f) => f.v);
  demo({
    id: "scrub", group: "Time", title: "Version scrub", relation: "the same thing at another time",
    sentence: "Nothing jumps: the head walks the releases between your pin and the one you chose, and every change lands as the head passes the release that made it, so watching the scrub is reading the history.",
    w: 1100, h: 720, dur: 2100, film: [0, 90, 180, 260, 330, 400, 470, 560, 700, 1500, 1700, 2000], crop: [0, 40, 1100, 460], interrupt: "reverse mid-scrub",
    spec: [["Driver", "one head position <code>s</code> in releases; click: tween to the target over <code>clamp(18 ms × releases + 120, 240, 640)</code>, curve <code>(.4,0,.2,1)</code>; drag: <code>s</code> = the pointer; Esc: back to the pin at 12 ms per release"],
      ["Beats", "each change is keyed to the release that made it (registry index for every release: Rust version, default dependencies; <code>releases.json</code> for the API of local ones). A beat plays when <code>s</code> crosses its tick, forwards or backwards"],
      ["Rolls", "the rider number rolls only the semver wheel that changed, one step per release crossed; counts roll one step per digit in numeric direction, ones first"],
      ["Arrive / leave", "clauses unroll from the left (160 ms glide); a dependency that goes is struck (120 ms) then closes (140 ms); the API section grows (220 ms, pushing what is below)"],
      ["Reduced", "the head and every value cut to the target; each changed token holds a mark (mint new, struck old) for 1.2 s"],
      ["GPUI", "<code>Motion::animate('scrub.s', Spec::tween)</code> for the head; beats = <code>Presence</code> (Axis::Horizontal for clauses, Vertical for rows); wheels = <b>Roll</b> (new)"]],
    build(o) {
      const st = stage(this.w, this.h);
      st.jb.innerHTML = segs([["package", "toml"], ["module", "value"], ["enum", "Value", true]]);
      st.addr.textContent = "nudox://toml/value/Value";
      const pin = vers.indexOf(TV.pinned), end = vers.length - 1, n = vers.length;
      const cw = 212;
      const x = (i) => (i / (n - 1)) * cw;
      const ticks = TV.versions.map((v, i) => {
        const prev = i ? TV.versions[i - 1].v.split(".") : null, cur = v.v.split(".");
        const hgt = !prev ? 12 : cur[0] !== prev[0] || (cur[0] === "0" && cur[1] !== prev[1]) ? 16 : cur[1] !== prev[1] ? 10 : 6;
        return `<i class="tk${v.local ? "" : " far"}${i === pin ? " pn" : ""}" data-i="${i}" style="left:${x(i).toFixed(2)}px;height:${i === pin ? 20 : hgt}px"></i>`;
      }).join("");
      st.shelf.innerHTML = `<div class="cup">${chev("s12", "transform:rotate(180deg)")}dependencies</div>
        <div class="cbook" style="padding-bottom:6px">${gem("package", 28).el.outerHTML}<div style="display:flex;flex-direction:column;gap:1px"><span class="bn">toml</span><span class="bv">${TV.pinned}</span></div></div>
        <div class="vc">${ticks}<span class="head"></span><span class="rider" style="position:absolute;bottom:23px;font:600 12px var(--mono);color:var(--peri-hi);white-space:nowrap"></span></div>
        <div class="vwrap" style="overflow:hidden"><div class="vline">you pin <span class="vpin">${TV.pinned}</span><kbd>esc</kbd></div></div>
        <div class="vsw" style="overflow:hidden;height:0"><div class="vsum"><span class="c0"></span> breaking · <span class="c1"></span> added · <span class="c2"></span> respelled<br>none of your ${TV.summary.uses} uses change</div></div>
        <div class="cfilter" style="margin-top:12px">${ico("filter", "s12")}<span>Filter</span></div>
        <div class="crows">${shelfRows([{ k: "module", n: "value" }, { k: "type", n: "Array", depth: 1 }, { k: "enum", n: "Value", depth: 1, cur: 1 }, { k: "macro", n: "impl_into_value", depth: 1 }, { k: "trait", n: "Index", depth: 1 }, { k: "trait", n: "Sealed", depth: 1 }, { k: "struct", n: "SeqDeserializer", depth: 1 }, { k: "struct", n: "MapDeserializer", depth: 1 }, { k: "struct", n: "ValueSerializer", depth: 1 }, { k: "struct", n: "TableSerializer", depth: 1 }, { k: "struct", n: "SerializeMap", depth: 1 }, { k: "struct", n: "DatetimeOrTable", depth: 1 }])}</div>`;
      $(st.shelf, ".crow.cur").style.background = "rgba(126,242,197,.05)";
      $(st.shelf, ".crow.cur").insertAdjacentHTML("afterbegin", `<i style="position:absolute;left:0;top:5px;bottom:5px;width:2px;background:var(--mint)"></i>`);
      const V = MD.valueToml;
      const variants = V.kids.filter((k) => k.k === "variant");
      const ds = TV.deserialize_struct;
      st.reader.innerHTML = `<div class="cfol" style="gap:22px">
        <div class="chero"><span class="hg"></span><div><div class="nm">Value</div><div class="ld">${esc(V.d)}</div></div></div>
        <div class="cfacts"><span>enum in <span class="mono">toml::value</span></span><i class="sep">·</i><span>used in 59 places</span><i class="sep">·</i><span><b>7</b> in your code</span></div>
        <div class="since lnote" style="height:0;overflow:hidden;margin-top:-8px"><span class="lead">built on</span> <span class="nmk">serde</span><span class="slot core"><span class="nmk nu">_core</span></span>, <span class="nmk">serde_spanned</span>, <span class="nmk">toml_datetime</span>, <span class="slot was"><span class="nmk old">toml_edit<i class="strike"></i></span></span><span class="slot neu"><span class="nmk nu">toml_parser</span>, <span class="nmk nu">toml_writer</span>, <span class="nmk nu">winnow</span></span> · needs Rust <span class="rust"></span></div>
        <div class="upw" style="height:0;overflow:hidden"><div class="up" style="padding-bottom:4px"><h3>Upgrading to <span class="upv"></span></h3>
          <div class="you">none of the ${TV.summary.uses} places your code uses toml change</div>
          <div class="moved">4 touch <code>toml::from_str</code>, respelled but the same in plain words: ${TV.sites.map((s) => `<code>${esc(s.file)}:${s.line}</code>`).join(", ")}</div>
          <div class="glrow gadd" style="margin-top:8px"><span class="gw">added</span><span class="n">${esc(ds.path.split("::").pop())}<i class="uline" style="left:0;right:0;bottom:3px;background:var(--mint)"></i></span><span class="s">(name <em>text</em>, _fields <em>list of text</em>, visitor <em>V</em>) → <em>Value</em></span></div></div></div>
        <div class="csec oneof"><div class="cgrp" style="padding-top:0">one of</div>${variants.map((v) => `<div class="ov"><span>${esc(v.n)}</span><span class="ty2">${esc(({ String: "text", Integer: "i64", Float: "f64", Boolean: "bool", Datetime: "Datetime", Array: "Array", Table: "Table" })[v.n] || "")}</span><span class="dsc">${esc(v.d)}</span></div>`).join("")}</div>
      </div>`;
      const g = gem("enum", 60); $(st.reader, ".hg").appendChild(g.el);
      const rider = odo("", "").states(vers, "version"); $(st.shelf, ".rider").appendChild(rider.el);
      const counts = [0, 1, 2].map((k) => { const o2 = odo().states(["0", String(TV.summary["1.1.6"][k])], "count"); $(st.shelf, `.c${k}`).appendChild(o2.el); return o2; });
      const upv = odo().states(["1.1.5", "1.1.6"], "version"); $(st.reader, ".upv").appendChild(upv.el);
      const rust = odo().states(["1.66", "1.76", "1.85"], "version"); $(st.reader, ".rust").appendChild(rust.el);
      return { root: st.root, st, g, rider, counts, upv, rust, pin, end, x, cw, after() {
        this.vwH = $(st.shelf, ".vwrap").scrollHeight; this.vsH = $(st.shelf, ".vsw").scrollHeight; this.upH = $(st.reader, ".upw").scrollHeight; this.sinceH = 22;
        this.slots = Object.fromEntries($$(st.reader, ".since .slot").map((e) => [e.classList[1], { el: e, w: e.getBoundingClientRect().width }]));
        this.addRow = $(st.reader, ".glrow.gadd"); this.addH = 26;
        this.ticks = $$(st.shelf, ".vc .tk");
      } };
    },
    frame(c, t, o) {
      const beats = { dep: vers.indexOf("0.9.0"), core: vers.indexOf("0.9.6"), r76: vers.indexOf("0.9.7"), r85: vers.indexOf("1.1.0"), api: vers.indexOf("1.1.5"), end: c.end };
      const n = c.end - c.pin;
      const D = clamp(18 * n + 120, 240, 640), back = clamp(12 * n + 120, 200, 520);
      const evs = o.int ? [[0, c.end, D, INOUT], [330, c.pin, 300, C.glide]] : [[0, c.end, D, INOUT], [1300, c.pin, back, INOUT]];
      let s = tween(c.pin, evs);
      if (o.reduced) s = (tt) => (o.int ? (tt < 330 ? c.end : c.pin) : tt < 1300 ? c.end : c.pin);
      const sv = s(t);
      // head, rider, passed ticks
      const hx = lerp(c.x(Math.floor(sv)), c.x(Math.min(c.end, Math.floor(sv) + 1)), sv - Math.floor(sv));
      const head = $(c.st.shelf, ".head"), rid = $(c.st.shelf, ".rider");
      const away = Math.abs(sv - c.pin) > 0.02;
      head.style.left = `${hx - 1}px`; head.style.opacity = away ? 1 : 0;
      c.rider.set(sv);
      const rw = c.rider.el.getBoundingClientRect().width;
      rid.style.left = `${hx > c.cw * 0.62 ? hx - rw - 6 : hx + 6}px`;
      rid.style.opacity = away ? 1 : 0;
      c.ticks.forEach((tk, i) => {
        if (i === c.pin) return;
        const lo = Math.min(c.pin, sv), hi = Math.max(c.pin, sv);
        const passed = i > lo && i <= hi + 0.001;
        const flash = passed ? clamp(1 - Math.abs(sv - i) / 1.6) : 0; // a tick lights as the head crosses it
        tk.style.background = passed ? mixc("#74819a", "#bcc6ff", flash) : "";
        tk.style.opacity = passed ? 1 : "";
      });
      $(c.st.shelf, ".bv").style.color = away ? "var(--ink4)" : "";
      // the shelf lens opens as soon as you leave the pin
      const lensOpen = beatTrack(crossings(s, [c.pin + 0.02], 2100)[0], 160);
      const lo = o.reduced ? (away ? 1 : 0) : lensOpen(t);
      $(c.st.shelf, ".vwrap").style.height = `${(c.vwH * lo).toFixed(1)}px`;
      // beats
      const X = crossings(s, [beats.dep, beats.core, beats.r76, beats.r85, beats.api, beats.api + 1], 2100);
      const R = (k, dur, curve) => (o.reduced ? (sv >= [beats.dep, beats.core, beats.r76, beats.r85, beats.api, beats.api + 1][k] ? 1 : 0) : beatTrack(X[k], dur, curve)(t));
      const b0 = R(0, 200), b1 = R(1, 180), b2 = R(2, 180), b3 = R(3, 180), b4 = R(4, 220), b5 = R(5, 200);
      // "built on …": the package at the viewed release. It opens with the lens, then only what changed moves
      const since = $(c.st.reader, ".since");
      since.style.height = `${(c.sinceH * lo).toFixed(1)}px`;
      const slot = (k, p) => { const q = c.slots[k]; q.el.style.maxWidth = `${(q.w * clamp(p)).toFixed(1)}px`; };
      // 0.9.0: toml_edit is struck, then closes, while its three successors unroll in its place
      $(c.slots.was.el, ".strike").style.transform = `scaleX(${clamp(b0 / 0.45).toFixed(3)})`;
      slot("was", 1 - band(b0, 0.45, 1));
      slot("neu", band(b0, 0.3, 1));
      // 0.9.6: serde grows its new syllable
      slot("core", b1);
      c.rust.set(b3 > 0 ? 1 + b3 : b2 > 0 ? 0 : 0);
      // API: the first local release after the pin
      const upw = $(c.st.reader, ".upw");
      upw.style.height = `${(c.upH * b4).toFixed(1)}px`;
      clip(upw.firstElementChild, 0, 0, 0, 0);
      c.upv.set(b5);
      c.addRow.style.height = `${(c.addH * clamp(b4 * 1.4 - 0.2)).toFixed(1)}px`;
      const ul = $(c.addRow, ".uline"); ul.style.transform = `scaleX(${clamp(b4 * 1.6 - 0.5).toFixed(3)})`;
      $(c.st.shelf, ".vsw").style.height = `${(c.vsH * clamp(b4 * 2)).toFixed(1)}px`;
      c.counts.forEach((w) => w.set(o.reduced ? b4 : band(b4, 0.15, 1)));
      if (o.reduced) {
        const mk = (el, on) => { if (el) el.style.boxShadow = on ? "inset 0 -1.5px 0 var(--mint)" : ""; };
        const m = markAt(t, 0) * (t < 1300 ? 1 : markAt(t, 1300));
        [$(c.st.reader, ".since"), $(c.st.reader, ".up h3")].forEach((el) => mk(el, m > 0.5));
      }
      c.g.light({});
    },
  });

  window.DEMO_HELPERS = { markAt, crossings, beatTrack, INOUT };
})();
