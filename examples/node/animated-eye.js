//
// An animated eye: a wink driven by springs rather than keyframes
//
// Run from a checkout:
//
//     just build            # or: npm run build
//     node examples/node/animated-eye.js [outdir]
//
// The require below is relative because the repo is not linked to itself.
// In your own project it is:  require("meo-skia-canvas")
//
// Nothing here is a keyframe. The lid, the pupil, the gaze, the brow and
// every one of the 200 lashes is a spring-damper integrated at a fixed
// 240 Hz with an accumulator, so the motion is a consequence of the forces
// rather than a curve someone drew:
//
//   - the lid spring is asymmetric -- stiff closing, soft opening -- so the
//     wink snaps shut and drifts back open, overshooting as it settles
//   - each lash lags the lid through its own spring, and its root angle is
//     blended from the open fan to a swept-down rest pose as the lid falls,
//     so the whole fan rotates outward and never sweeps through the eye
//   - lid velocity draws a second ghost copy of each lash: motion blur that
//     costs nothing and appears only during the snap
//   - the eyeball rolls up as the lid closes (Bell's phenomenon), so the
//     iris is seen climbing out of view mid-wink
//   - the catchlights sit on the cornea rather than in the iris plane, so
//     they track the gaze at roughly half speed -- the parallax is what
//     makes the eye read as a dome instead of a disc
//
// The drawing leans on four things this library has that a browser canvas
// does not: `Path2D.jitter()` for hand-drawn edges on every hair and fibre,
// `MaskFilter` for the soft occlusion in the socket and under the lashes,
// a Display P3 canvas for the iris blues, and `toFile("...avif")` writing the
// animation straight out of the canvas's pages.
//

const fs = require("fs");
const path = require("path");
const { Canvas, Path2D, MaskFilter } = require("../../lib");

const OUT = process.argv[2] || "out";
fs.mkdirSync(OUT, { recursive: true });

const W = 640,
  H = 500,
  FRAMES = 150,
  FPS = 60,
  DT = 1 / 240;
const CX = W / 2,
  CY = H / 2 + 26;
const R = 88; // iris radius
// GPU by default, which is what you want for anything this stroke-heavy.
// Set `MEO_EYE_CPU=1` to force the raster path: the two renderers antialias
// differently -- measurably, 19% of bytes on the same drawing -- so pinning
// the CPU is what makes a committed asset byte-identical between machines.
// That matters for a regenerated file in a repo and for nothing else.
const canvas = new Canvas(W, H, {
  colorSpace: "display-p3",
  gpu: !process.env.MEO_EYE_CPU,
});

// ── palette ─────────────────────────────────────────────────────────────
const SKIN = "#f2d3c6",
  SKIN_DEEP = "#e2ad9d",
  CREASE = "#c08a7c";
const SHADOW_SKIN = "#c9998b",
  LID_TINT = "#dba396";
const BROW = "#5f4230",
  BROW_LIT = "#96704e",
  BROW_DK = "#453022";
const WATER = "#c94f52",
  SCLERA = "#f5efec",
  SCLERA_SHADE = "#c9bcb9";
const DEEP = "#12405c",
  MID = "#2f83ad",
  PALE = "#a8dcec",
  GOLD = "#c8963a";
const LASH = "#100b0b";

// ── deterministic noise ─────────────────────────────────────────────────
const R1 = (i) => {
  const s = Math.sin(i * 12.9898) * 43758.5453;
  return s - Math.floor(s);
};

// ── physics state ───────────────────────────────────────────────────────
const pupil = { r: 26, v: 0 };
const gaze = { x: 0, y: 0, vx: 0, vy: 0, tx: 0, ty: 0 };
const lid = { open: 1, v: 0, target: 1 };
const brow = { y: 0, v: 0 }; // wink dip
const LASHN = 200;
const lashLag = Array.from({ length: LASHN }, () => ({ a: 0, v: 0 }));
let SQ = 0; // orbicularis squeeze, per frame

function step(t, winkDepth) {
  const light =
    0.5 +
    0.3 * Math.sin(t * Math.PI * 2) +
    0.3 * Math.exp(-40 * ((t % 1) - 0.5) ** 2);
  const want = 34 - light * 16;
  pupil.v += (-90 * (pupil.r - want) - 14 * pupil.v) * DT;
  pupil.r += pupil.v * DT;
  pupil.r = Math.max(12, Math.min(40, pupil.r));

  gaze.vx += (-150 * (gaze.x - gaze.tx) - 17 * gaze.vx) * DT;
  gaze.vy += (-150 * (gaze.y - gaze.ty) - 17 * gaze.vy) * DT;
  gaze.x += gaze.vx * DT;
  gaze.y += gaze.vy * DT;

  // Asymmetric lid spring: the close is a snap, the reopen is soft and
  // underdamped, so it overshoots -- which is what reads as dramatic.
  const closing = lid.target < lid.open;
  const k = closing ? 460 : 190,
    c = closing ? 27 : 17;
  lid.v += (-k * (lid.open - lid.target) - c * lid.v) * DT;
  lid.open += lid.v * DT;
  lid.open = Math.max(-0.02, Math.min(1.1, lid.open));

  brow.v += (-120 * (brow.y - winkDepth * 13) - 14 * brow.v) * DT;
  brow.y += brow.v * DT;

  // Lashes lag the lid through their own springs; the gain is high enough
  // that the snap-close whips them and the reopen flutters.
  for (const l of lashLag) {
    l.v += (-200 * l.a - 12 * l.v + lid.v * 7.5) * DT;
    l.a += l.v * DT;
    l.a = Math.max(-0.65, Math.min(0.65, l.a));
  }
}

// ── geometry ────────────────────────────────────────────────────────────
const IN_X = CX - 160,
  OUT_X = CX + 160;
const atX = (u) => IN_X + (OUT_X - IN_X) * u;

function lowerY(u) {
  // The squeeze lifts the lower lid -- a wink engages it, a blink barely.
  return (
    CY + Math.sin(u * Math.PI) ** 0.68 * 79 - SQ * 26 * Math.sin(u * Math.PI)
  );
}
function upperY(u, open) {
  // Exponent below one fills the arc out: a full dome, not a pointed wedge.
  const arc = Math.sin(u * Math.PI) ** 0.74;
  const wide = CY - arc * 97;
  const shut = lowerY(u) - 1.5;
  return shut + (wide - shut) * open;
}
function creaseY(u, open) {
  const rest = upperY(u, 1) - 30 - Math.sin(u * Math.PI) * 12;
  // as the lid closes the fold chases it down
  return rest + (upperY(u, open) - rest) * (1 - open) * 0.5;
}
function openingPath(open) {
  const p = new Path2D();
  p.moveTo(IN_X, lowerY(0) - 1);
  for (let i = 1; i <= 48; i++) {
    const u = i / 48;
    p.lineTo(atX(u), upperY(u, open));
  }
  for (let i = 47; i >= 0; i--) {
    const u = i / 48;
    p.lineTo(atX(u), lowerY(u));
  }
  p.closePath();
  return p;
}

// A lash: a closed tapered sliver -- thick at the root, a point at the tip.
function lashPath(x, y, ang, len, curl, wide) {
  const mx = x + Math.cos(ang) * len * 0.55,
    my = y + Math.sin(ang) * len * 0.55;
  const tx = mx + Math.cos(ang + curl) * len * 0.6,
    ty = my + Math.sin(ang + curl) * len * 0.6;
  const nx = -Math.sin(ang) * wide,
    ny = Math.cos(ang) * wide;
  const p = new Path2D();
  p.moveTo(x - nx, y - ny);
  p.quadraticCurveTo(mx - nx * 0.4, my - ny * 0.4, tx, ty);
  p.quadraticCurveTo(mx + nx * 0.4, my + ny * 0.4, x + nx, y + ny);
  p.closePath();
  return p;
}

// Root angle for an upper lash: the open fan blended toward a swept-down
// resting pose as the lid closes, rotating outward -- never through the eye.
function upperLashAim(u, open, lag) {
  // Where a lash points, and how long it looks, as the lid falls.
  //
  // The fan is mirror-symmetric about the middle of the lid: the further
  // left a lash sits the further left it leans, and the same to the right.
  // That has to hold at every stage of the wink, not just at the ends, and
  // it is the reason this interpolates a direction *vector* rather than an
  // angle. Angles cannot do it. Turning every lash the same way collapses
  // the fan to near-horizontal halfway down -- a 0.22 rad spread, all of it
  // leaning one way. Letting each half turn its own way is symmetric but
  // meets in a seam at the centre, where the two directions are 180 apart
  // and the shorter way round flips sign.
  //
  // A vector lerp has neither problem: each side crosses through its own
  // side as it falls, so the fan spreads open halfway and closes again, and
  // nothing is discontinuous anywhere.
  const openAng = -Math.PI / 2 - 0.62 + u * 1.24;
  const shutAng = -openAng; // the same fan, mirrored downward
  const k = 1 - Math.pow(Math.max(0, Math.min(1, open)), 0.75);

  let x = Math.cos(openAng) * (1 - k) + Math.cos(shutAng) * k;
  let y = Math.sin(openAng) * (1 - k) + Math.sin(shutAng) * k;

  // The centre lash points at the viewer halfway down, which in a flat
  // drawing is a vector of length zero. A little downward bias, peaking at
  // the halfway point, gives it somewhere to be -- and the shortening that
  // survives is the foreshortening a lash pointing outward really has.
  y += Math.sin(Math.PI * k) * 0.55;

  return { ang: Math.atan2(y, x) + lag, squash: Math.hypot(x, y) };
}

// ── render ──────────────────────────────────────────────────────────────
let acc = 0;
for (let f = 0; f < FRAMES; f++) {
  const ctx = f ? canvas.newPage() : canvas.getContext("2d");
  const t = f / FRAMES;

  // — timeline: saccades, one quick blink, then the dramatic wink —
  if (t < 0.005) {
    gaze.tx = 0;
    gaze.ty = 0;
  }
  if (t > 0.1 && t < 0.11) {
    gaze.tx = -26;
    gaze.ty = -8;
  }
  if (t > 0.26 && t < 0.27) {
    gaze.tx = 20;
    gaze.ty = 6;
  }
  if (t > 0.44 && t < 0.45) {
    gaze.tx = 0;
    gaze.ty = 0;
  }

  let target = 1;
  const nb = Math.abs(t - 0.34); // quick natural blink
  if (nb < 0.045) {
    const q = nb / 0.045;
    target = q * q * (3 - 2 * q);
  }
  if (t >= 0.55 && t < 0.62)
    target = 1.07; // anticipation: widen
  else if (t >= 0.62 && t < 0.655) {
    // snap shut
    const q = (t - 0.62) / 0.035;
    target = 1.07 * (1 - q * q * (3 - 2 * q));
  } else if (t >= 0.655 && t < 0.76)
    target = 0; // hold
  else if (t >= 0.76 && t < 0.88) {
    // slow reopen
    const q = (t - 0.76) / 0.12;
    target = q * q * (3 - 2 * q);
  }
  lid.target = target;
  const winkDepth =
    t > 0.56 && t < 0.97 ? Math.max(0, 1 - Math.max(0, lid.open)) : 0;

  acc += 1 / FPS;
  while (acc >= DT) {
    step(t, winkDepth);
    acc -= DT;
  }
  SQ = winkDepth * 0.9;
  const open = lid.open,
    lidVel = lid.v;
  // The eye is never still: a slow two-frequency drift keeps it alive.
  const micro = Math.sin(f * 0.63) * 0.7 + Math.sin(f * 1.71 + 2) * 0.5;
  // Bell's phenomenon: the eyeball rolls up as the lid closes, so what you
  // glimpse mid-wink is the iris climbing out of view.
  const roll = (1 - Math.max(0, Math.min(1, open))) * 20;
  const gx = CX + gaze.x + micro,
    gy = CY - 12 + gaze.y - roll;
  // A reflection sits on the cornea, not in the iris plane, so it tracks
  // the gaze at roughly half speed -- that parallax is what sells the dome.
  const clx = CX + gaze.x * 0.45 + micro * 0.5;
  const cly = CY - 12 + gaze.y * 0.45 - roll * 0.35;

  // — paper —
  ctx.fillStyle = "#f6f2ee";
  ctx.fillRect(0, 0, W, H);

  // — skin mass —
  ctx.save();
  ctx.maskFilter = new MaskFilter("normal", 28);
  const skin = ctx.createRadialGradient(CX, CY - 14, 40, CX, CY - 14, 265);
  skin.addColorStop(0, SKIN_DEEP);
  skin.addColorStop(0.5, SKIN);
  skin.addColorStop(1, "rgba(242 211 198 / 0)");
  ctx.fillStyle = skin;
  ctx.beginPath();
  ctx.ellipse(CX, CY - 18, 235, 168, 0, 0, 7);
  ctx.fill();
  ctx.restore();

  // — under-eye subsurface tint, and the highlight along the brow bone —
  ctx.save();
  ctx.maskFilter = new MaskFilter("normal", 18);
  ctx.globalAlpha = 0.22;
  ctx.fillStyle = "#c99aa4";
  ctx.beginPath();
  ctx.ellipse(CX, lowerY(0.5) + 44, 165, 30, 0, 0, 7);
  ctx.fill();
  ctx.globalAlpha = 0.35;
  ctx.fillStyle = "#fdeee2";
  ctx.beginPath();
  ctx.ellipse(CX + 20, CY - 158 + brow.y * 0.5, 150, 22, -0.04, 0, 7);
  ctx.fill();
  ctx.restore();
  ctx.globalAlpha = 1;

  // pore stipple, fixed seed so it does not crawl frame to frame
  ctx.save();
  for (let i = 0; i < 420; i++) {
    const rx = R1(i * 3.1) * 2 - 1,
      ry = R1(i * 5.7 + 1) * 2 - 1;
    const x = CX + rx * 250,
      y = CY - 30 + ry * 190;
    if (((x - CX) / 195) ** 2 + ((y - CY) / 105) ** 2 < 1) continue; // not on the eye
    ctx.globalAlpha = 0.03 + R1(i * 7.7) * 0.05;
    ctx.fillStyle = R1(i * 2.3) > 0.5 ? "#b98a7c" : "#fde8dc";
    ctx.beginPath();
    ctx.arc(x, y, 0.7 + R1(i * 9.1) * 1.1, 0, 7);
    ctx.fill();
  }
  ctx.restore();
  ctx.globalAlpha = 1;

  // — nose-bridge shadow on the inner side, temple shading on the outer —
  ctx.save();
  ctx.maskFilter = new MaskFilter("normal", 26);
  ctx.fillStyle = "#dda595";
  ctx.globalAlpha = 0.3;
  ctx.beginPath();
  ctx.ellipse(IN_X - 66, CY + 4, 36, 82, 0.1, 0, 7);
  ctx.fill();
  ctx.fillStyle = "#e0ac9b";
  ctx.globalAlpha = 0.18;
  ctx.beginPath();
  ctx.ellipse(OUT_X + 72, CY - 6, 30, 78, -0.12, 0, 7);
  ctx.fill();
  ctx.restore();

  // crow's feet at the outer corner: faint at rest, cut deep by the squeeze
  ctx.save();
  ctx.maskFilter = new MaskFilter("normal", 1.8);
  ctx.strokeStyle = "#b97f70";
  ctx.lineCap = "round";
  for (let i = 0; i < 4; i++) {
    const rnd = R1(i * 21 + 4);
    const a = -0.32 + i * 0.24 + (rnd - 0.5) * 0.08;
    const x0 = OUT_X + 4,
      y0 = CY - 6 + (i - 1.5) * 10;
    const len = (26 + rnd * 22) * (1 + winkDepth * 0.6);
    ctx.globalAlpha = 0.12 + winkDepth * 0.3;
    ctx.lineWidth = 1.6 + winkDepth * 1.2;
    const cf = new Path2D();
    cf.moveTo(x0, y0);
    cf.quadraticCurveTo(
      x0 + Math.cos(a) * len * 0.6,
      y0 + Math.sin(a) * len * 0.55,
      x0 + Math.cos(a) * len,
      y0 + Math.sin(a) * len,
    );
    ctx.stroke(cf.jitter(5, 1.4, i * 43));
  }
  ctx.restore();

  // the fine under-eye crease where lid meets cheek
  ctx.save();
  ctx.maskFilter = new MaskFilter("normal", 2.2);
  ctx.strokeStyle = "#c08a7c";
  ctx.globalAlpha = 0.22 + winkDepth * 0.2;
  ctx.lineWidth = 2.4;
  const uel = new Path2D();
  for (let i = 4; i <= 40; i++) {
    const u = i / 44;
    const y = lowerY(u) + 20 + Math.sin(u * Math.PI) * 6 - winkDepth * 8;
    i === 4 ? uel.moveTo(atX(u), y) : uel.lineTo(atX(u), y);
  }
  ctx.stroke(uel.jitter(8, 1.6, f * 3 + 9));
  ctx.restore();
  ctx.globalAlpha = 1;

  // — cheek pushed up by the wink —
  if (winkDepth > 0.02) {
    ctx.save();
    ctx.maskFilter = new MaskFilter("normal", 22);
    ctx.globalAlpha = winkDepth * 0.5;
    ctx.fillStyle = "#e8a795";
    ctx.beginPath();
    ctx.ellipse(CX + 10, lowerY(0.5) + 46 - winkDepth * 12, 150, 40, 0, 0, 7);
    ctx.fill();
    ctx.restore();
  }

  // — brow: shadow understory, then hairs in three zones —
  const browBase = (u) => {
    const rise =
      u < 0.62
        ? Math.sin(((u / 0.62) * Math.PI) / 2)
        : Math.cos(((((u - 0.62) / 0.38) * Math.PI) / 2) * 0.9);
    return CY - 208 - rise * 34 + u * 20 + brow.y + winkDepth * 6 * (1 - u);
  };
  // The socket is a cavity. Both shadow bands are drawn as short segments
  // whose alpha rises and falls along the brow, because a stroke with a
  // blurred square end reads as a smudge floating past the hairs.
  const shadowBand = (dy, width, blur, color, peak) => {
    ctx.save();
    ctx.maskFilter = new MaskFilter("normal", blur);
    ctx.strokeStyle = color;
    ctx.lineWidth = width;
    ctx.lineCap = "round";
    for (let i = 0; i < 22; i++) {
      const u0 = 0.05 + (i / 22) * 0.9,
        u1 = 0.05 + ((i + 1.4) / 22) * 0.9;
      const envelope =
        Math.sin((((u0 + u1) / 2 - 0.05) / 0.9) * Math.PI) ** 0.55;
      ctx.globalAlpha = peak * envelope;
      const seg = new Path2D();
      seg.moveTo(IN_X - 20 + (OUT_X - IN_X + 62) * u0, browBase(u0) + dy);
      seg.lineTo(IN_X - 20 + (OUT_X - IN_X + 62) * u1, browBase(u1) + dy);
      ctx.stroke(seg);
    }
    ctx.restore();
  };
  shadowBand(44, 42, 22, "#d6a08f", 0.42); // broad orbital shading
  shadowBand(16, 13, 8, "#bd8676", 0.5); // the brow's own cast line
  ctx.save();
  ctx.lineCap = "round";
  for (let i = 0; i < 300; i++) {
    const rnd = R1(i * 1.7 + 3),
      u = i / 300;
    const bx = IN_X - 28 + (OUT_X - IN_X + 78) * u + (rnd - 0.5) * 8;
    const by = browBase(u) + (rnd - 0.5) * (20 - u * 8);
    // head hairs stand nearly upright, the body angles over, the tail lies flat
    const zone = u < 0.16 ? -1.25 : -0.92 + (u - 0.16) * 1.35;
    const ang = zone + (rnd - 0.5) * 0.3;
    const len = (22 + rnd * 30) * (u < 0.16 ? 1.15 : 1 - u * 0.42);
    ctx.strokeStyle = rnd > 0.75 ? BROW_LIT : rnd > 0.25 ? BROW : BROW_DK;
    ctx.globalAlpha = (0.3 + rnd * 0.5) * (u > 0.82 ? 1 - (u - 0.82) * 3.5 : 1);
    ctx.lineWidth = 0.9 + rnd * 1.9;
    const h = new Path2D();
    h.moveTo(bx, by);
    h.quadraticCurveTo(
      bx + Math.cos(ang) * len * 0.55,
      by + Math.sin(ang) * len * 0.5,
      bx + Math.cos(ang + 0.4) * len,
      by + Math.sin(ang + 0.4) * len,
    );
    ctx.stroke(h.jitter(6, 1.3, i * 11));
  }
  ctx.restore();
  ctx.globalAlpha = 1;

  // — crease, deepening as the lid folds —
  ctx.save();
  ctx.maskFilter = new MaskFilter("normal", 6);
  ctx.strokeStyle = CREASE;
  ctx.lineWidth = 8 + winkDepth * 4;
  ctx.globalAlpha = 0.75 + winkDepth * 0.25;
  const crease = new Path2D();
  for (let i = 0; i <= 36; i++) {
    const u = i / 36;
    i
      ? crease.lineTo(atX(u), creaseY(u, open))
      : crease.moveTo(atX(u), creaseY(u, open));
  }
  ctx.stroke(crease.jitter(9, 1.5, f * 13 + 5));
  // lid plate tint between crease and lash line
  ctx.maskFilter = new MaskFilter("normal", 11);
  ctx.strokeStyle = LID_TINT;
  ctx.globalAlpha = 0.35;
  ctx.lineWidth = 24;
  const plate = new Path2D();
  for (let i = 0; i <= 36; i++) {
    const u = i / 36;
    const y = (creaseY(u, open) + upperY(u, open)) / 2;
    i ? plate.lineTo(atX(u), y) : plate.moveTo(atX(u), y);
  }
  ctx.stroke(plate);
  ctx.restore();
  ctx.globalAlpha = 1;

  // extra bunched folds while the wink squeezes
  if (winkDepth > 0.05) {
    ctx.save();
    ctx.globalAlpha = 0.16 * winkDepth;
    ctx.strokeStyle = CREASE;
    ctx.lineWidth = 2;
    for (let i = 0; i < 8; i++) {
      const rnd = R1(i * 7 + 2),
        u0 = 0.15 + rnd * 0.6;
      const w = new Path2D();
      w.moveTo(atX(u0), creaseY(u0, open) - 8 - rnd * 14);
      w.quadraticCurveTo(
        atX(u0 + 0.12),
        creaseY(u0 + 0.1, open) - 20 - rnd * 12,
        atX(u0 + 0.22),
        creaseY(u0 + 0.2, open) - 10 - rnd * 10,
      );
      ctx.stroke(w.jitter(6, 2, i * 31));
    }
    ctx.restore();
  }

  // — waterline —
  const opening = openingPath(open);
  ctx.save();
  ctx.maskFilter = new MaskFilter("normal", 3.5);
  ctx.strokeStyle = WATER;
  ctx.lineWidth = 7;
  ctx.stroke(opening);
  ctx.restore();

  // — eyeball —
  ctx.save();
  ctx.clip(opening);

  const sc = ctx.createRadialGradient(gx - 20, gy - 16, 24, CX, CY, 225);
  sc.addColorStop(0, "#ffffff");
  sc.addColorStop(0.42, SCLERA);
  sc.addColorStop(1, SCLERA_SHADE);
  ctx.fillStyle = sc;
  ctx.fillRect(0, 0, W, H);
  // pink corner shading
  for (const cxr of [IN_X + 18, OUT_X - 18]) {
    const g = ctx.createRadialGradient(cxr, CY, 4, cxr, CY, 76);
    g.addColorStop(0, "rgba(224 142 134 / 0.5)");
    g.addColorStop(1, "rgba(224 142 134 / 0)");
    ctx.fillStyle = g;
    ctx.fillRect(cxr - 80, CY - 80, 160, 160);
  }
  // vasculature, forking, from both corners
  ctx.strokeStyle = "#b4483f";
  for (let i = 0; i < 24; i++) {
    const rnd = R1(i * 91.7),
      side = i % 2 ? 1 : -1;
    const x0 = side < 0 ? IN_X + 6 : OUT_X - 6,
      y0 = CY - 30 + rnd * 60;
    ctx.globalAlpha = 0.08 + rnd * 0.2;
    ctx.lineWidth = 0.7 + rnd * 1.4;
    const v = new Path2D();
    v.moveTo(x0, y0);
    v.quadraticCurveTo(
      x0 + side * (40 + rnd * 42),
      y0 - 24 + rnd * 46,
      x0 + side * (88 + rnd * 60),
      y0 - 6 + rnd * 28,
    );
    ctx.stroke(v.jitter(5, 2.6, i * 13 + 3));
    if (rnd > 0.55) {
      const b = new Path2D();
      b.moveTo(x0 + side * 44, y0 - 6);
      b.quadraticCurveTo(x0 + side * 74, y0 + 16, x0 + side * 106, y0 + 8);
      ctx.globalAlpha *= 0.7;
      ctx.stroke(b.jitter(5, 2.4, i * 29));
    }
  }
  ctx.globalAlpha = 1;

  // — iris —
  const iris = new Path2D();
  iris.arc(gx, gy, R, 0, 7);
  ctx.save();
  ctx.clip(iris);
  const base = ctx.createRadialGradient(gx, gy, pupil.r * 0.8, gx, gy, R);
  base.addColorStop(0, "#1d5c7d");
  base.addColorStop(0.3, MID);
  base.addColorStop(0.72, "#5aa8c8");
  base.addColorStop(1, DEEP);
  ctx.fillStyle = base;
  ctx.fillRect(gx - R, gy - R, R * 2, R * 2);

  ctx.lineCap = "round";
  const LAYERS = [
    { n: 260, lo: "#0d3550", hi: "#1b5c7f", w: 3.4, a: 0.5, s: 3 },
    { n: 340, lo: "#3f95bd", hi: PALE, w: 2.0, a: 0.55, s: 11 },
    { n: 140, lo: "#cdeef8", hi: "#ffffff", w: 1.2, a: 0.4, s: 29 },
  ];
  for (const L of LAYERS) {
    for (let i = 0; i < L.n; i++) {
      const rnd = R1(i + L.s),
        a = (i / L.n) * Math.PI * 2 + rnd * 0.03;
      const inner = pupil.r + 2 + rnd * 9,
        outer = R * (0.64 + rnd * 0.38);
      ctx.strokeStyle = rnd > 0.5 ? L.hi : L.lo;
      ctx.globalAlpha = L.a * (0.45 + rnd * 0.7);
      ctx.lineWidth = L.w * (0.4 + rnd);
      const fib = new Path2D();
      fib.moveTo(gx + Math.cos(a) * inner, gy + Math.sin(a) * inner);
      fib.lineTo(gx + Math.cos(a) * outer, gy + Math.sin(a) * outer);
      ctx.stroke(fib.jitter(5, 2.1, i * 31 + L.s * 7));
    }
  }
  // crypts: dark radial pits in the stroma
  ctx.save();
  ctx.maskFilter = new MaskFilter("normal", 2);
  ctx.fillStyle = "#0e3247";
  for (let i = 0; i < 16; i++) {
    const rnd = R1(i * 4.4 + 9),
      a = (i / 16) * Math.PI * 2 + rnd;
    const rr = pupil.r + 16 + rnd * (R * 0.5);
    ctx.globalAlpha = 0.22 + rnd * 0.2;
    ctx.save();
    ctx.translate(gx + Math.cos(a) * rr, gy + Math.sin(a) * rr);
    ctx.rotate(a);
    ctx.beginPath();
    ctx.ellipse(0, 0, 5 + rnd * 9, 2.5 + rnd * 3, 0, 0, 7);
    ctx.fill();
    ctx.restore();
  }
  ctx.restore();
  // contraction furrows: concentric arcs
  ctx.strokeStyle = "#123a52";
  for (const [ri, rr] of [
    [0, pupil.r + 24],
    [1, pupil.r + 38],
    [2, R * 0.82],
  ]) {
    ctx.globalAlpha = 0.2;
    ctx.lineWidth = 1.6;
    const fur = new Path2D();
    fur.arc(gx, gy, Math.min(rr, R - 6), 0, 7);
    ctx.stroke(fur.jitter(6, 2.4, 60 + ri * 17));
  }
  // collarette: gold sunburst spokes
  for (let i = 0; i < 72; i++) {
    const rnd = R1(i * 55.3),
      a = (i / 72) * Math.PI * 2;
    ctx.strokeStyle = rnd > 0.5 ? "#d9a441" : GOLD;
    ctx.globalAlpha = 0.55 + rnd * 0.45;
    ctx.lineWidth = 2 + rnd * 3.4;
    const sp = new Path2D();
    sp.moveTo(
      gx + Math.cos(a) * (pupil.r + 1),
      gy + Math.sin(a) * (pupil.r + 1),
    );
    sp.lineTo(
      gx + Math.cos(a) * (pupil.r + 14 + rnd * 15),
      gy + Math.sin(a) * (pupil.r + 14 + rnd * 15),
    );
    ctx.stroke(sp.jitter(4, 1.8, i * 23));
  }
  ctx.globalAlpha = 1;
  // caustic: light dumped through the lens onto the far side of the iris
  ctx.save();
  ctx.maskFilter = new MaskFilter("normal", 9);
  ctx.fillStyle = "rgba(190 238 252 / 0.4)";
  ctx.beginPath();
  ctx.ellipse(gx + R * 0.34, gy + R * 0.44, R * 0.5, R * 0.3, 0.5, 0, 7);
  ctx.fill();
  ctx.restore();
  // limbal ring: blurred, because a real limbus is a gradient not a stroke
  ctx.save();
  ctx.maskFilter = new MaskFilter("normal", 5);
  ctx.strokeStyle = "#0b1d2b";
  ctx.lineWidth = 15;
  ctx.globalAlpha = 0.9;
  ctx.beginPath();
  ctx.arc(gx, gy, R - 3, 0, 7);
  ctx.stroke();
  ctx.restore();
  // corneal sheen: a faint film of light over the upper iris
  const sheen = ctx.createLinearGradient(0, gy - R, 0, gy + R * 0.35);
  sheen.addColorStop(0, "rgba(255 255 255 / 0.15)");
  sheen.addColorStop(1, "rgba(255 255 255 / 0)");
  ctx.fillStyle = sheen;
  ctx.fillRect(gx - R, gy - R, R * 2, R * 1.4);
  ctx.globalAlpha = 1;
  ctx.restore(); // end iris clip

  // pupil
  ctx.fillStyle = "#060406";
  ctx.beginPath();
  ctx.arc(gx, gy, pupil.r, 0, 7);
  ctx.fill();
  ctx.save();
  ctx.maskFilter = new MaskFilter("normal", 3);
  ctx.strokeStyle = "rgba(6 4 6 / 0.7)";
  ctx.lineWidth = 4;
  ctx.beginPath();
  ctx.arc(gx, gy, pupil.r + 1.5, 0, 7);
  ctx.stroke();
  ctx.restore();

  // the upper lid's shadow falling on the ball, tracking the lid itself
  ctx.save();
  ctx.maskFilter = new MaskFilter("normal", 15);
  ctx.fillStyle = "rgba(64 34 28 / 0.4)";
  const shBand = new Path2D();
  for (let i = 0; i <= 36; i++) {
    const u = i / 36;
    i
      ? shBand.lineTo(atX(u), upperY(u, open) + 12)
      : shBand.moveTo(atX(u), upperY(u, open) + 12);
  }
  for (let i = 36; i >= 0; i--) {
    const u = i / 36;
    shBand.lineTo(atX(u), upperY(u, open) - 26);
  }
  shBand.closePath();
  ctx.fill(shBand);
  ctx.restore();

  // wet meniscus along the lower lid, plus one sparkle
  ctx.save();
  ctx.maskFilter = new MaskFilter("normal", 1.6);
  ctx.strokeStyle = "rgba(255 255 255 / 0.5)";
  ctx.lineWidth = 2.4;
  const men = new Path2D();
  for (let i = 4; i <= 44; i++) {
    const u = i / 48;
    i === 4
      ? men.moveTo(atX(u), lowerY(u) - 3)
      : men.lineTo(atX(u), lowerY(u) - 3);
  }
  ctx.stroke(men);
  ctx.restore();
  ctx.fillStyle = "rgba(255 255 255 / 0.85)";
  ctx.beginPath();
  ctx.arc(atX(0.72), lowerY(0.72) - 5, 2.2, 0, 7);
  ctx.fill();

  // catchlights: window shapes, drawn over the pupil edge
  ctx.save();
  ctx.maskFilter = new MaskFilter("normal", 7);
  ctx.fillStyle = "rgba(190 225 255 / 0.6)";
  ctx.beginPath();
  ctx.ellipse(clx - 34, cly - 36, 19, 15, -0.5, 0, 7);
  ctx.fill();
  ctx.restore();
  ctx.fillStyle = "rgba(255 255 255 / 0.97)";
  ctx.beginPath();
  ctx.roundRect(clx - 44, cly - 46, 19, 17, 2.5);
  ctx.fill();
  ctx.beginPath();
  ctx.roundRect(clx - 22, cly - 26, 10, 9, 2);
  ctx.fill();
  ctx.fillStyle = "rgba(255 255 255 / 0.45)";
  ctx.beginPath();
  ctx.ellipse(clx + 30, cly + 34, 8, 5, 0.4, 0, 7);
  ctx.fill();

  ctx.restore(); // end opening clip

  // — tear duct: the lids close over it, so it fades with the opening —
  const ductA = Math.max(0, Math.min(1, (open - 0.18) / 0.45));
  if (ductA > 0.02) {
    ctx.save();
    ctx.globalAlpha = ductA;
    ctx.maskFilter = new MaskFilter("normal", 3);
    const duct = new Path2D();
    duct.moveTo(IN_X + 2, CY - 14);
    duct.quadraticCurveTo(IN_X + 34, CY - 6, IN_X + 30, CY + 10);
    duct.quadraticCurveTo(IN_X + 16, CY + 20, IN_X + 2, CY - 14);
    duct.closePath();
    ctx.fillStyle = "#d4726f";
    ctx.fill(duct);
    ctx.fillStyle = "#f2b0a8";
    ctx.beginPath();
    ctx.ellipse(IN_X + 16, CY - 1, 8, 6, -0.3, 0, 7);
    ctx.fill();
    ctx.restore();
    ctx.globalAlpha = 1;
  }

  // — lid margins —
  ctx.strokeStyle = "#8e3f3d";
  ctx.lineWidth = 3.5;
  ctx.stroke(openingPath(open));
  // dark lash-line shelf on the upper lid
  ctx.save();
  ctx.maskFilter = new MaskFilter("normal", 2.5);
  ctx.strokeStyle = "rgba(40 18 18 / 0.8)";
  ctx.lineWidth = 6;
  const shelf = new Path2D();
  for (let i = 0; i <= 40; i++) {
    const u = i / 40;
    i
      ? shelf.lineTo(atX(u), upperY(u, open) - 3)
      : shelf.moveTo(atX(u), upperY(u, open) - 3);
  }
  ctx.stroke(shelf.jitter(7, 1.2, f * 7 + 41));
  // light platform under the lower lashes
  ctx.maskFilter = new MaskFilter("normal", 4);
  ctx.strokeStyle = "rgba(248 222 210 / 0.55)";
  ctx.lineWidth = 7;
  const ledge = new Path2D();
  for (let i = 2; i <= 38; i++) {
    const u = i / 40;
    i === 2
      ? ledge.moveTo(atX(u), lowerY(u) + 8)
      : ledge.lineTo(atX(u), lowerY(u) + 8);
  }
  ctx.stroke(ledge);
  ctx.restore();

  // — upper lashes: fill row, main row, hero row; all rotate with the lid —
  ctx.fillStyle = LASH;
  const ghost = Math.min(0.35, Math.abs(lidVel) * 0.05);
  const upperRow = (count, seedOff, lenMul, wideMul, alpha, lagGain) => {
    for (let i = 0; i < count; i++) {
      const rnd = R1(i * 3.3 + seedOff);
      const u = 0.04 + (i / (count - 1)) * 0.93;
      const x = atX(u),
        y = upperY(u, open);
      const grow = Math.sin(u * Math.PI) ** 0.3;
      const len = (44 + rnd * 34) * grow * (0.5 + u * 0.85) * lenMul;
      const idx = (i * 7 + seedOff) % LASHN;
      // Real lashes gather into clumps of three or four whose tips converge;
      // a shared per-clump bias does that without modelling adhesion.
      const clump = (R1((i >> 2) * 13 + seedOff) - 0.5) * 0.17;
      const aim = upperLashAim(
        u,
        open,
        lashLag[idx].a * lagGain + clump + (rnd - 0.5) * 0.06,
      );
      const ang = aim.ang;
      // Foreshortened as it swings through the viewer: a lash pointing at
      // the camera is short on the page, which is most of why the fan reads
      // as sitting on a curved lid rather than a flat one.
      const shown = len * aim.squash;
      const curl =
        (0.6 + rnd * 0.32) * (0.35 + 0.65 * Math.max(0, Math.min(1, open)));
      const wide = (1.4 + rnd * 1.8) * grow * wideMul;
      ctx.globalAlpha = alpha * (0.75 + rnd * 0.25);
      ctx.fill(lashPath(x, y + 1, ang, shown, curl, wide));
      if (ghost > 0.04) {
        // motion blur on the snap
        ctx.globalAlpha = ghost * alpha;
        ctx.fill(lashPath(x, y + 1, ang - lidVel * 0.016, shown, curl, wide));
      }
    }
  };
  upperRow(64, 5, 0.6, 0.7, 0.55, 0.8); // short fill behind
  upperRow(78, 17, 1.0, 1.0, 0.95, 1.0); // main row
  upperRow(24, 45, 1.5, 1.25, 1.0, 1.3); // long hero lashes

  // — lower-lash shadows cast on the skin, a few px below their owners —
  if (open > 0.5) {
    ctx.save();
    ctx.maskFilter = new MaskFilter("normal", 2.5);
    ctx.fillStyle = "rgba(94 48 42 / 0.16)";
    for (let i = 0; i < 34; i += 2) {
      const rnd = R1(i * 78.2),
        u = 0.12 + (i / 33) * 0.8;
      const x = atX(u),
        y = lowerY(u) + 7;
      const grow = Math.sin(u * Math.PI) ** 0.5;
      const len = (17 + rnd * 15) * grow;
      const ang = Math.PI / 2 - 0.42 + u * 0.9;
      ctx.fill(lashPath(x, y, ang, len, 0.24, 1.1 * grow));
    }
    ctx.restore();
  }

  // — lower lashes: shorter, sparser, pressed down slightly by the squeeze —
  for (let i = 0; i < 34; i++) {
    const rnd = R1(i * 78.2),
      u = 0.12 + (i / 33) * 0.8;
    const x = atX(u),
      y = lowerY(u);
    const grow = Math.sin(u * Math.PI) ** 0.5;
    const len = (17 + rnd * 15) * grow * (1 - winkDepth * 0.25);
    const ang =
      Math.PI / 2 -
      0.5 +
      u * 0.95 +
      lashLag[(i * 5 + 90) % LASHN].a * 0.5 +
      winkDepth * 0.3;
    ctx.globalAlpha = 0.6 + rnd * 0.3;
    ctx.fill(lashPath(x, y - 1, ang, len, 0.3, (0.9 + rnd * 0.8) * grow));
  }
  ctx.globalAlpha = 1;

  // — film grain, reseeded each frame: the shimmer of a drawn frame —
  ctx.save();
  for (let i = 0; i < 260; i++) {
    const gxr = R1(i * 1.9 + f * 37.7) * W,
      gyr = R1(i * 4.3 + f * 91.1) * H;
    ctx.globalAlpha = 0.028;
    ctx.fillStyle = R1(i + f) > 0.5 ? "#3a2c28" : "#ffffff";
    ctx.fillRect(gxr, gyr, 1.4, 1.4);
  }
  ctx.restore();
  ctx.globalAlpha = 1;

  // — vignette into the paper —
  const vig = ctx.createRadialGradient(CX, CY - 20, 210, CX, CY - 20, 400);
  vig.addColorStop(0, "rgba(246 242 238 / 0)");
  vig.addColorStop(1, "rgba(246 242 238 / 0.9)");
  ctx.fillStyle = vig;
  ctx.fillRect(0, 0, W, H);
}

// Three formats, and the differences between them are arithmetic rather
// than taste.
//
// AVIF is the showcase because it is the smallest by a wide margin: the same
// 150 frames are 2.7 MB against WebP's 4.7 and the GIF's 12.2. It carries
// 24-bit colour and the canvas's Display P3 profile, and it codes each frame
// against the ones before it rather than storing every frame whole, which is
// where most of that saving comes from on a drawing that moves this little
// between frames.
//
// What AVIF does not fix is the timing. Its container counts in ticks of a
// 90 kHz clock, which could express a 60fps frame exactly -- but this
// library's frame delays are whole milliseconds all the way through, so it
// stores 16 and 17 like WebP does. The only format here that is exact at
// 60fps is APNG, which stores the delay as a fraction, and it cost 34 MB to
// be right about a third of a millisecond a frame. This example stopped
// writing one.
//
// The GIF stays because it is the one anything will play, and because what
// it costs is worth seeing: a frame delay in hundredths of a second, so a
// 60fps frame -- 16.67ms -- is not a whole number of them. The delays are
// handed out as differences between running totals rather than rounded one
// at a time, so the average comes out right, but the frames alternate
// between 10 and 20ms and the format cannot do better. Its palette is 256
// entries a frame, and this drawing is mostly smooth gradient -- skin,
// sclera, iris -- which is what banding shows up in worst.
for (const format of ["avif", "webp", "gif"]) {
  const file = path.join(OUT, `animated-eye.${format}`);
  canvas.toFileSync(file, { fps: FPS, loop: 0 });
  console.log(
    `animated-eye.${format}`.padEnd(20),
    `${canvas.width}x${canvas.height}`,
    // The GIF is written at the rate it was asked for and the file says so.
    // Browsers will not play it: anything at or under 10ms is rendered at
    // 100ms, and 60fps needs frames of 16.67. Named here rather than left
    // for someone to discover from a GIF that limps -- and named as the
    // browsers' behaviour, because nothing in this library caps anything
    // and a native viewer will play it as written.
    `${FRAMES} frames @ ${FPS}fps` +
      (format === "gif" ? " (browsers play <=10ms frames at 100ms)" : ""),
    (fs.statSync(file).size / 1024 / 1024).toFixed(1) + " MB",
  );
}
