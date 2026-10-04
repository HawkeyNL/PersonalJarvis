<script setup lang="ts">
import { computed, onBeforeUnmount, onMounted, ref } from "vue";
import type { Tone } from "../../admin";

// The Jarvis orb from the design export, rebuilt for WebKitGTK: no SVG
// turbulence filter, no blurred/blended blobs (plain radial gradients), and
// every animation touches only transform or opacity. Core Admin shows it
// tinted by overall Home Node health; `still` stops every animation (the
// dashboard is a status read-out, not an assistant). Animations also pause
// while the window is hidden and stop under prefers-reduced-motion.
const props = withDefaults(defineProps<{ size?: number; tone?: Tone; still?: boolean; label?: boolean }>(), {
  size: 346,
  tone: "ok",
  still: false,
  label: true,
});

const hidden = ref(typeof document !== "undefined" && document.hidden);
const onVisibility = () => { hidden.value = document.hidden; };
onMounted(() => document.addEventListener("visibilitychange", onVisibility));
onBeforeUnmount(() => document.removeEventListener("visibilitychange", onVisibility));

const style = computed(() => ({
  width: props.size + "px",
  height: props.size + "px",
  "--f": "1",
  "--label": Math.max(13, Math.round(props.size * 0.055)) + "px",
}));

const sparksA = [
  [50, 0, 4, 0], [85, 15, 3, -1.1], [99, 58, 4, -2.3], [70, 96, 3, -0.6],
  [22, 91, 4, -1.8], [1, 44, 3, -2.7], [16, 12, 2, -0.3],
];
const sparksB = [[30, 4, 2, -1.4], [96, 34, 3, -2.1], [80, 90, 2, -0.9], [4, 68, 3, -2.9]];
const veins = [
  "M50 50C42 40 35 38 24 30", "M50 50C58 41 66 36 77 28", "M50 50C60 55 70 62 80 70",
  "M50 50C41 58 32 64 22 72", "M50 50C50 62 47 74 49 88", "M50 50C51 38 54 24 50 12",
];
const branches = [
  "M35 38C30 46 22 48 14 52", "M66 36C72 44 82 46 88 48", "M70 62C66 72 62 80 60 88",
  "M32 64C28 56 20 58 12 60", "M54 24C62 20 68 16 74 12", "M42 40C40 30 36 22 30 16",
  "M47 74C40 78 34 84 30 90",
];
const sigTiming = [[3.4, 0], [4.1, -1.3], [3.8, -2.2], [4.6, -0.7], [3.1, -1.9], [4.3, -2.8]];
const nodes = [
  [35, 38, 1.1, 0], [66, 36, 1.1, -0.4], [70, 62, 1.1, -0.9], [32, 64, 1.1, -1.3],
  [54, 24, 1, -1.7], [47, 74, 1, -2.1], [24, 30, 0.8, -0.6], [77, 28, 0.8, -1.1],
  [80, 70, 0.8, -1.5], [22, 72, 0.8, -1.9], [14, 52, 0.7, -2.4], [88, 48, 0.7, -0.2],
];
</script>

<template>
  <div class="jv-orb" :class="['tone-' + tone, { paused: hidden, still }]" :style="style" aria-hidden="true">
    <div class="halo"></div>

    <div class="ring-sparks a">
      <span v-for="(s, i) in sparksA" :key="i" class="spark"
        :style="{ left: s[0] + '%', top: s[1] + '%', width: s[2] + 'px', height: s[2] + 'px', margin: -s[2] / 2 + 'px', animationDelay: s[3] + 's' }"></span>
    </div>
    <div class="ring-sparks b">
      <span v-for="(s, i) in sparksB" :key="i" class="spark"
        :style="{ left: s[0] + '%', top: s[1] + '%', width: s[2] + 'px', height: s[2] + 'px', margin: -s[2] / 2 + 'px', animationDelay: s[3] + 's' }"></span>
    </div>

    <div class="membrane outer"></div>
    <div class="membrane main"></div>
    <div class="membrane inner"></div>

    <div class="beat">
      <div class="sphere">
        <div class="blob da"></div>
        <div class="blob db"></div>
        <div class="blob dc"></div>
        <svg viewBox="0 0 100 100" class="veins">
          <g fill="none" stroke="rgba(210,255,235,.26)" stroke-width=".45" stroke-linecap="round">
            <path v-for="d in veins" :key="d" :d="d" />
            <path v-for="d in branches" :key="d" :d="d" />
          </g>
          <g fill="none" stroke="#eafff5" stroke-width="1" stroke-linecap="round" stroke-dasharray="4 60">
            <path v-for="(d, i) in veins" :key="d" class="sig" :d="d"
              :style="{ animationDuration: `calc(${sigTiming[i][0]}s * var(--f))`, animationDelay: sigTiming[i][1] + 's' }" />
          </g>
          <g fill="#dfffee">
            <circle v-for="(n, i) in nodes" :key="i" class="node" :cx="n[0]" :cy="n[1]" :r="n[2]"
              :style="{ animationDelay: n[3] + 's' }" />
          </g>
        </svg>
        <div class="nucleus"></div>
        <div class="gloss"></div>
      </div>
    </div>

    <div v-if="label" class="label">
      <span>JARVIS</span>
      <span class="bar"></span>
    </div>
  </div>
</template>

<style scoped>
.jv-orb { position: relative; flex: none; }
.jv-orb > div, .beat > div, .sphere > div { position: absolute; border-radius: 50%; }

.halo {
  left: -30%; top: -30%; width: 160%; height: 160%;
  background: radial-gradient(circle, rgba(var(--accent-rgb), 0.34) 0, rgba(var(--accent-rgb), 0.12) 36%, transparent 62%);
  animation: jvBreathe 6s ease-in-out infinite;
}
.ring-sparks.a { left: -11%; top: -11%; width: 122%; height: 122%; animation: jvSpin 60s linear infinite; }
.ring-sparks.b { left: -22%; top: -22%; width: 144%; height: 144%; opacity: 0.7; animation: jvSpin 95s linear infinite reverse; }
.spark {
  position: absolute; border-radius: 50%; background: #b8ffdf;
  box-shadow: 0 0 7px var(--accent);
  animation: jvTwinkle 3.2s ease-in-out infinite;
}

/* Membranes: slightly irregular fixed shapes that rotate, which reads as the
   export's border-radius morph without animating layout or paint. */
.membrane { box-sizing: border-box; }
.membrane.outer {
  left: -2%; top: -2%; width: 104%; height: 104%;
  border: 1px solid rgba(150, 255, 210, 0.3);
  border-radius: 46% 54% 51% 49% / 52% 45% 55% 48% !important;
  animation: jvSpin 14s ease-in-out infinite;
}
.membrane.main {
  inset: 0;
  border: 1.5px solid rgba(150, 255, 210, 0.5);
  border-radius: 49% 51% 52% 48% / 51% 48% 52% 49% !important;
  box-shadow: 0 0 22px rgba(var(--accent-rgb), 0.4), inset 0 0 22px rgba(var(--accent-rgb), 0.22);
  animation: jvSpin 14s ease-in-out infinite;
}
.membrane.inner {
  left: 3%; top: 3%; width: 94%; height: 94%;
  border: 1px solid rgba(var(--accent-rgb), 0.38);
  border-radius: 52% 48% 47% 53% / 47% 54% 46% 53% !important;
  background: radial-gradient(circle, transparent 60%, rgba(var(--accent-rgb), 0.12) 100%);
  animation: jvSpin 19s ease-in-out infinite reverse;
}

.beat {
  left: 6.5%; top: 6.5%; width: 87%; height: 87%;
  animation: jvBeat calc(3.2s * var(--f)) ease-out infinite;
}
.sphere {
  inset: 0; overflow: hidden;
  background: radial-gradient(circle at 42% 38%, #3ff0a2 0, #14a868 28%, #0a6744 56%, #05301f 84%, #03190f 100%);
  box-shadow: 0 0 70px rgba(var(--accent-rgb), 0.55), inset 0 0 40px rgba(150, 255, 210, 0.35), inset -20px -30px 60px rgba(0, 0, 0, 0.45);
  animation: jvSwell 9s ease-in-out infinite;
}
.blob { will-change: transform; }
.da { left: 6%; top: 10%; width: 56%; height: 56%; background: radial-gradient(circle, rgba(170, 255, 215, 0.5), rgba(170, 255, 215, 0.16) 45%, transparent 70%); animation: jvDriftA 9s ease-in-out infinite alternate; }
.db { left: 44%; top: 46%; width: 50%; height: 50%; background: radial-gradient(circle, rgba(90, 255, 175, 0.42), rgba(90, 255, 175, 0.12) 45%, transparent 70%); animation: jvDriftB 12s ease-in-out infinite alternate; }
.dc { left: 14%; top: 52%; width: 40%; height: 40%; background: radial-gradient(circle, rgba(205, 255, 232, 0.34), rgba(205, 255, 232, 0.1) 45%, transparent 70%); animation: jvDriftC 15s ease-in-out infinite alternate; }
.veins { position: absolute; inset: 0; width: 100%; height: 100%; }
.sig { opacity: 0; animation: jvSignal 4s ease-in-out infinite; }
.node { animation: jvNode 2.6s ease-in-out infinite; }
.nucleus {
  left: 28%; top: 28%; width: 44%; height: 44%;
  background: radial-gradient(circle, rgba(200, 255, 228, 0.7) 0, rgba(80, 240, 165, 0.3) 32%, rgba(80, 240, 165, 0.08) 52%, transparent 68%);
  animation: jvNucleus calc(4s * var(--f)) ease-in-out infinite;
}
.gloss {
  left: 16%; top: 10%; width: 36%; height: 22%;
  background: radial-gradient(ellipse, rgba(255, 255, 255, 0.32), transparent 70%);
  transform: rotate(-22deg);
}

.label {
  inset: 0; border-radius: 0 !important;
  display: flex; flex-direction: column; align-items: center; justify-content: center;
  font-family: var(--font-display); font-weight: 600; font-size: var(--label);
  letter-spacing: 0.4em; padding-left: 0.4em; color: var(--text-0);
  text-shadow: 0 0 12px rgba(0, 30, 20, 0.85), 0 0 2px rgba(0, 30, 20, 0.9);
}
.label .bar {
  margin-top: 0.47em; margin-left: -0.4em; width: 2.6em; height: 2px;
  background: rgba(244, 255, 250, 0.85); box-shadow: 0 0 8px rgba(0, 30, 20, 0.8);
}

@keyframes jvBreathe { 0%, 100% { opacity: 0.6; transform: scale(0.97); } 50% { opacity: 1; transform: scale(1.05); } }
@keyframes jvSpin { to { transform: rotate(360deg); } }
@keyframes jvTwinkle { 0%, 100% { opacity: 0.2; } 50% { opacity: 1; } }
@keyframes jvSwell { 0%, 100% { transform: scale(1); } 33% { transform: scale(1.012, 0.99); } 66% { transform: scale(0.992, 1.01); } }
@keyframes jvBeat { 0%, 100% { transform: scale(1); } 10% { transform: scale(1.03); } 20% { transform: scale(0.995); } 30% { transform: scale(1.018); } 45% { transform: scale(1); } }
@keyframes jvDriftA { 0% { transform: translate(0, 0) scale(1); } 50% { transform: translate(30%, 20%) scale(1.15); } 100% { transform: translate(-8%, 34%) scale(0.9); } }
@keyframes jvDriftB { 0% { transform: translate(0, 0) scale(1); } 50% { transform: translate(-34%, -18%) scale(0.85); } 100% { transform: translate(-10%, -38%) scale(1.1); } }
@keyframes jvDriftC { 0% { transform: translate(0, 0) scale(0.9); } 50% { transform: translate(40%, -26%) scale(1.2); } 100% { transform: translate(20%, 10%) scale(1); } }
@keyframes jvSignal { 0%, 100% { opacity: 0; } 40%, 60% { opacity: 0.9; } }
@keyframes jvNode { 0%, 100% { opacity: 0.3; } 50% { opacity: 1; } }
@keyframes jvNucleus { 0%, 100% { opacity: 0.5; transform: scale(0.88); } 50% { opacity: 0.95; transform: scale(1.14); } }

/* Health tint: the accent triplet drives halo, sparks and membranes; the
   sphere gets its own gradient per tone. */
.tone-warn { --accent-rgb: 245, 184, 74; --accent: #f5b84a; }
.tone-error { --accent-rgb: 248, 113, 113; --accent: #f87171; }
.tone-idle { --accent-rgb: 137, 149, 160; --accent: #8995a0; }
.tone-warn .sphere { background: radial-gradient(circle at 42% 38%, #ffd88a 0, #d99a2b 28%, #8a5a12 56%, #3d2706 84%, #1d1303 100%); }
.tone-error .sphere { background: radial-gradient(circle at 42% 38%, #ffb0b0 0, #d9534f 28%, #8a2424 56%, #3d0c0c 84%, #1d0505 100%); }
.tone-idle .sphere { background: radial-gradient(circle at 42% 38%, #c9d3db 0, #7d8a95 28%, #4a555e 56%, #232a30 84%, #12161a 100%); }

.paused, .paused * { animation-play-state: paused !important; }
.still, .still * { animation: none !important; }
.still .sig { opacity: 0.5; }
@media (prefers-reduced-motion: reduce) {
  .jv-orb, .jv-orb * { animation: none !important; }
  .sig { opacity: 0.5; }
}
</style>
