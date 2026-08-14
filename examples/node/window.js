//
// A window, animated and driven by events
//
// Run from a checkout:
//
//     just build            # or: npm run build
//     node examples/node/window.js
//
// The Node half of `examples/rust/window.rs`. Same drawing, same two
// handlers, so the files read as a translation of each other.
//
// Needs a display. This is the one example that cannot run headless: it opens
// a real window and blocks until it is closed.
//
// The require below is relative because the repo is not linked to itself.
// In your own project it is:  require("meo-skia-canvas")
//

const { Window } = require("../../lib");

const W = 480,
  H = 320;

const win = new Window(W, H, {
  title: "meo-skia-canvas",
  background: "#101014",
});

// Follows the pointer, so it is visibly reacting to events rather than just
// animating on a timer.
//
// A plain closure variable is enough here: JavaScript closures share one
// binding, so both handlers see the same `pointer`. The Rust mirror needs an
// `Rc<Cell<_>>` for this, because each `move` closure would otherwise capture
// its own copy.
let pointer = { x: W / 2, y: H / 2 };

win.on("mousemove", ({ x, y }) => {
  pointer = { x, y };
});

// Escape closes the window, which ends the loop once it is the last one.
win.on("keydown", ({ key }) => {
  if (key === "Escape") win.close();
});

win.on("draw", ({ frame }) => {
  const ctx = win.ctx;
  ctx.fillStyle = "#0f0f14";
  ctx.fillRect(0, 0, W, H);

  // The frame counter is handed in rather than tracked here, so the animation
  // does not need any state of its own.
  const phase = frame / 30;
  const radius = 40 + 12 * Math.sin(phase);

  ctx.fillStyle = "skyblue";
  ctx.beginPath();
  ctx.arc(pointer.x, pointer.y, radius, 0, Math.PI * 2);
  ctx.fill();
});
