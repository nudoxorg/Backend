// Motion for the moments board: one spring family, arcs, and a frame loop.
// CARRY (response 0.28 s, critically damped) is the house spring; PLAY adds a small overshoot for
// things that land (a card, a block making room). Text never scales; marks and frames travel.

export const clamp = (v, a = 0, b = 1) => Math.min(b, Math.max(a, v));
export const lerp = (a, b, t) => a + (b - a) * t;
export const ease = {
  out: (t) => 1 - Math.pow(1 - t, 3),
  inOut: (t) => (t < 0.5 ? 4 * t * t * t : 1 - Math.pow(-2 * t + 2, 3) / 2),
  in: (t) => t * t * t,
};

// A spring as progress over time: response = period of the undamped oscillation (s), zeta = damping ratio.
export function spring(response = 0.28, zeta = 1) {
  const w = (2 * Math.PI) / response;
  if (zeta >= 1) return (ms) => (ms <= 0 ? 0 : 1 - (1 + w * (ms / 1000)) * Math.exp(-w * (ms / 1000)));
  const wd = w * Math.sqrt(1 - zeta * zeta);
  return (ms) => {
    if (ms <= 0) return 0;
    const t = ms / 1000;
    return 1 - Math.exp(-zeta * w * t) * (Math.cos(wd * t) + ((zeta * w) / wd) * Math.sin(wd * t));
  };
}
export const CARRY = spring(0.28, 1);
export const PLAY = spring(0.42, 0.72);   // lands with ~6% overshoot
export const ROOM = spring(0.32, 0.8);    // neighbours making room
export const settle = (ms, response = 0.28) => ms > response * 1000 * 2.2;

// A point along a quadratic arc from a to b that bows `lift` px above the chord (toward -y).
export function arc(a, b, u, lift) {
  const mx = (a.x + b.x) / 2, my = (a.y + b.y) / 2 - lift;
  const v = 1 - u;
  return { x: v * v * a.x + 2 * v * u * mx + u * u * b.x, y: v * v * a.y + 2 * v * u * my + u * u * b.y };
}
// The arc's heading at u, in radians (for leaning a card into its flight).
export function heading(a, b, u, lift) {
  const mx = (a.x + b.x) / 2, my = (a.y + b.y) / 2 - lift;
  const dx = 2 * (1 - u) * (mx - a.x) + 2 * u * (b.x - mx), dy = 2 * (1 - u) * (my - a.y) + 2 * u * (b.y - my);
  return Math.atan2(dy, dx);
}

// One shared frame loop. Each task is fn(now) → true while it wants more frames. A frame comes from rAF,
// or from a timer when rAF is starved (headless capture under virtual time), so films are deterministic.
const tasks = new Set();
let running = false;
function frame(cb) {
  let done = false;
  requestAnimationFrame(() => { if (!done) { done = true; cb(performance.now()); } });
  setTimeout(() => { if (!done) { done = true; cb(performance.now()); } }, 34);
}
function loop(now) {
  for (const t of [...tasks]) if (!t(now)) tasks.delete(t);
  if (tasks.size) frame(loop); else running = false;
}
export function every(fn) {
  tasks.add(fn);
  if (!running) { running = true; frame(loop); }
  return () => tasks.delete(fn);
}
// Runs fn(ms since start) for `dur` ms (or until fn returns false); resolves when done.
export function play(dur, fn) {
  return new Promise((done) => {
    const t0 = performance.now();
    every((now) => {
      const ms = now - t0;
      const more = fn(Math.min(ms, dur)) !== false && ms < dur;
      if (!more) done();
      return more;
    });
  });
}
export const wait = (ms) => new Promise((r) => setTimeout(r, ms));

// A value that chases its target on a spring, for hover states (lift, dim, swell).
export class Chase {
  constructor(v = 0, rate = 18) { this.v = v; this.to = v; this.rate = rate; }
  step(dt) { this.v += (this.to - this.v) * (1 - Math.exp(-this.rate * dt)); return Math.abs(this.to - this.v) > 0.001; }
}
