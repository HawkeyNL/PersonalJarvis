<script setup lang="ts">
// A "living" Jarvis-style reactor core: a canvas particle field (an energy
// fountain), rotating SVG rings + ticks, and a conic radar sweep. Purely
// presentational — the readout text is passed in via props.
import { ref, onMounted, onBeforeUnmount } from "vue";
import { accentRgb } from "../theme";

withDefaults(
  defineProps<{
    name?: string;
    active?: boolean;
  }>(),
  {
    name: "Jarvis",
    active: true,
  },
);

const canvas = ref<HTMLCanvasElement | null>(null);
let raf = 0;
let ctx: CanvasRenderingContext2D | null = null;

const SIZE = 360; // internal drawing resolution (square user units)
const N = 150;
let cr = 96,
  cg = 255,
  cb = 184; // particle colour (follows the accent)

type P = { a: number; r: number; life: number; max: number; sp: number; sz: number };
const parts: P[] = [];

function reset(p: P) {
  p.a = Math.random() * Math.PI * 2;
  p.r = Math.random() * 66;
  p.max = 55 + Math.random() * 120;
  p.life = Math.random() * p.max;
  p.sp = 0.3 + Math.random() * 0.9;
  p.sz = 0.5 + Math.random() * 1.9;
}

function init() {
  parts.length = 0;
  for (let i = 0; i < N; i++) {
    const p: P = { a: 0, r: 0, life: 0, max: 0, sp: 0, sz: 0 };
    reset(p);
    parts.push(p);
  }
}

function drawFrame() {
  if (!ctx) return;
  const c = ctx;
  const cx = SIZE / 2;
  const cy = SIZE / 2;
  c.clearRect(0, 0, SIZE, SIZE);

  c.globalCompositeOperation = "lighter";
  for (const p of parts) {
    p.life += p.sp;
    if (p.life > p.max) reset(p);
    const t = p.life / p.max;
    const rr = p.r * (0.6 + t * 1.0);
    const x = cx + Math.cos(p.a) * rr;
    const y = cy + Math.sin(p.a) * rr * 0.55 - t * 34; // slight upward drift
    const alpha = Math.sin(t * Math.PI) * 0.8;
    c.fillStyle = `rgba(${cr}, ${cg}, ${cb}, ${alpha.toFixed(3)})`;
    c.beginPath();
    c.arc(x, y, p.sz, 0, Math.PI * 2);
    c.fill();
  }

  // soft central bloom
  const g = c.createRadialGradient(cx, cy, 0, cx, cy, 96);
  g.addColorStop(0, `rgba(${cr}, ${cg}, ${cb}, 0.20)`);
  g.addColorStop(1, `rgba(${cr}, ${cg}, ${cb}, 0)`);
  c.fillStyle = g;
  c.fillRect(0, 0, SIZE, SIZE);
  c.globalCompositeOperation = "source-over";

  raf = requestAnimationFrame(drawFrame);
}

onMounted(() => {
  const el = canvas.value;
  if (!el) return;
  const dpr = Math.min(window.devicePixelRatio || 1, 2);
  el.width = SIZE * dpr;
  el.height = SIZE * dpr;
  ctx = el.getContext("2d");
  if (ctx) ctx.scale(dpr, dpr);
  [cr, cg, cb] = accentRgb();
  init();

  const reduce = window.matchMedia?.("(prefers-reduced-motion: reduce)").matches;
  if (reduce) drawFrame(); // one static frame, no loop
  else raf = requestAnimationFrame(drawFrame);
});

onBeforeUnmount(() => cancelAnimationFrame(raf));
</script>

<template>
  <div class="core" :class="{ idle: !active }">
    <canvas ref="canvas" class="core-canvas"></canvas>
    <div class="core-sweep"></div>

    <svg class="core-svg" viewBox="0 0 360 360" aria-hidden="true">
      <g class="ticks">
        <line
          v-for="n in 60"
          :key="n"
          x1="180"
          :y1="n % 5 === 1 ? 18 : 22"
          x2="180"
          y2="28"
          :transform="`rotate(${(n - 1) * 6} 180 180)`"
        />
      </g>

      <circle class="ring dim" cx="180" cy="180" r="156" />
      <circle class="ring dim" cx="180" cy="180" r="96" />

      <circle class="ring spin-a" cx="180" cy="180" r="168" stroke-dasharray="2 10" />
      <circle class="ring spin-b" cx="180" cy="180" r="140" stroke-dasharray="42 16 8 16" />
      <circle class="ring spin-c" cx="180" cy="180" r="112" stroke-dasharray="1 7" />

      <circle class="arc spin-d" cx="180" cy="180" r="162" stroke-dasharray="80 940" />
      <circle class="arc spin-e" cx="180" cy="180" r="126" stroke-dasharray="46 700" />
    </svg>

    <div class="core-readout">
      <div class="core-name">{{ name }}</div>
    </div>
  </div>
</template>

<style scoped>
.core {
  position: relative;
  width: min(var(--core-size, 400px), 90vw);
  aspect-ratio: 1;
  margin: 0 auto;
  filter: drop-shadow(0 0 28px rgba(52, 245, 160, 0.16));
}

.core-canvas,
.core-svg,
.core-sweep,
.core-readout {
  position: absolute;
  inset: 0;
}

.core-canvas {
  width: 100%;
  height: 100%;
}

.core-sweep {
  border-radius: 50%;
  background: conic-gradient(
    from 0deg,
    transparent 0 300deg,
    rgba(52, 245, 160, 0.05) 322deg,
    rgba(52, 245, 160, 0.34) 356deg,
    transparent 360deg
  );
  -webkit-mask: radial-gradient(circle, transparent 30%, #000 54%);
  mask: radial-gradient(circle, transparent 30%, #000 54%);
  animation: sweep 6s linear infinite;
}

.core-svg {
  width: 100%;
  height: 100%;
  overflow: visible;
}

.ticks line {
  stroke: rgba(125, 255, 192, 0.35);
  stroke-width: 1.4;
}

.ring {
  fill: none;
  stroke: var(--accent);
  stroke-width: 1.4;
  transform-box: fill-box;
  transform-origin: center;
}

.ring.dim {
  stroke: rgba(52, 245, 160, 0.14);
  stroke-width: 1;
}

.arc {
  fill: none;
  stroke: var(--accent-2);
  stroke-width: 2.2;
  stroke-linecap: round;
  transform-box: fill-box;
  transform-origin: center;
  filter: drop-shadow(0 0 4px rgba(125, 255, 192, 0.6));
}

.spin-a { animation: spin 44s linear infinite; }
.spin-b { animation: spin 30s linear infinite reverse; }
.spin-c { animation: spin 20s linear infinite; }
.spin-d { animation: spin 9s linear infinite; }
.spin-e { animation: spin 14s linear infinite reverse; }

.core-readout {
  display: flex;
  flex-direction: column;
  align-items: center;
  justify-content: center;
  text-align: center;
  pointer-events: none;
}

.core-name {
  font-size: clamp(34px, 8vh, 66px);
  font-weight: 700;
  line-height: 1;
  letter-spacing: 0.14em;
  color: #eafff4;
  text-shadow: 0 0 22px rgba(52, 245, 160, 0.55), 0 0 6px rgba(52, 245, 160, 0.4);
  text-indent: 0.14em; /* balance the trailing letter-spacing */
}

.core.idle {
  filter: grayscale(0.5) drop-shadow(0 0 10px rgba(120, 120, 120, 0.1));
  opacity: 0.8;
}

@keyframes spin { to { transform: rotate(360deg); } }
@keyframes sweep { to { transform: rotate(360deg); } }
@keyframes blink { 0%, 100% { opacity: 1; } 50% { opacity: 0.25; } }

@media (prefers-reduced-motion: reduce) {
  .core-sweep,
  .spin-a, .spin-b, .spin-c, .spin-d, .spin-e,
  .core-dot { animation: none; }
}
</style>
