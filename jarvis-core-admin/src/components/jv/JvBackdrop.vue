<script setup lang="ts">
// Static space backdrop: stars, a soft glow behind the orb and the planet
// horizon. Pure CSS backgrounds, painted once; nothing here animates.
withDefaults(defineProps<{ glowY?: string; horizon?: string }>(), {
  glowY: "43%",
  horizon: "189px",
});
</script>

<template>
  <div class="jv-backdrop" aria-hidden="true" :style="{ '--glow-y': glowY, '--horizon': horizon }">
    <div class="stars"></div>
    <div class="planet"></div>
    <div class="flare"></div>
  </div>
</template>

<style scoped>
.jv-backdrop {
  position: fixed;
  inset: 0;
  z-index: 0;
  overflow: hidden;
  pointer-events: none;
  background:
    radial-gradient(ellipse 620px 460px at 50% var(--glow-y), rgba(20, 150, 95, 0.22), transparent 70%),
    radial-gradient(ellipse 1100px 220px at 50% 100%, rgba(30, 200, 120, 0.18), transparent 70%),
    var(--bg);
}
.stars {
  position: absolute;
  inset: 0;
  --s: #bfffe6;
  --g: #7dffc8;
  background:
    radial-gradient(circle at 67.5% 19%, var(--g) 0 1.5px, rgba(var(--accent-rgb), 0.5) 2px, transparent 5px),
    radial-gradient(circle at 32.6% 61%, var(--g) 0 1.5px, rgba(var(--accent-rgb), 0.5) 2px, transparent 5px),
    radial-gradient(circle at 69.5% 40%, var(--g) 0 1.5px, rgba(var(--accent-rgb), 0.5) 2px, transparent 5px),
    radial-gradient(circle at 81.5% 75%, var(--s) 0 1.5px, transparent 2px),
    radial-gradient(circle at 1.6% 70%, var(--s) 0 1px, transparent 1.5px),
    radial-gradient(circle at 87.2% 22%, var(--s) 0 1px, transparent 1.5px),
    radial-gradient(circle at 29.3% 19.4%, var(--s) 0 1px, transparent 1.5px),
    radial-gradient(circle at 14% 13.5%, var(--s) 0 1px, transparent 1.5px),
    radial-gradient(circle at 76.8% 12%, var(--s) 0 1px, transparent 1.5px),
    radial-gradient(circle at 91.9% 39.6%, var(--s) 0 1px, transparent 1.5px),
    radial-gradient(circle at 10.4% 44.2%, var(--s) 0 1px, transparent 1.5px),
    radial-gradient(circle at 5.9% 32.2%, var(--s) 0 1px, transparent 1.5px),
    radial-gradient(circle at 89.8% 58.9%, var(--s) 0 1px, transparent 1.5px),
    radial-gradient(circle at 20% 62.6%, var(--s) 0 1px, transparent 1.5px),
    radial-gradient(circle at 74.2% 68.6%, var(--s) 0 1px, transparent 1.5px),
    radial-gradient(circle at 23.5% 12%, var(--s) 0 1px, transparent 1.5px),
    radial-gradient(circle at 4.1% 81%, var(--s) 0 1px, transparent 1.5px);
  opacity: 0.75;
}
.planet {
  position: absolute;
  left: -29%;
  width: 158%;
  top: calc(100% - var(--horizon));
  height: 800px;
  border-radius: 50%;
  background:
    radial-gradient(circle at 70% 9%, #9bffd6 0 1px, transparent 1.5px),
    radial-gradient(circle at 72% 12%, #9bffd6 0 1.5px, transparent 2px),
    radial-gradient(circle at 74% 15%, #9bffd6 0 1px, transparent 1.5px),
    radial-gradient(circle at 66% 13%, #9bffd6 0 1px, transparent 1.5px),
    radial-gradient(circle at 42% 10%, #9bffd6 0 1px, transparent 1.5px),
    radial-gradient(circle at 38% 14%, #9bffd6 0 1px, transparent 1.5px),
    radial-gradient(ellipse at 50% 0%, rgba(40, 160, 100, 0.35) 0%, #031510 12%, #010806 55%);
  border-top: 2px solid rgba(150, 255, 210, 0.7);
  box-shadow: 0 -10px 50px rgba(var(--accent-rgb), 0.45), 0 -3px 160px rgba(var(--accent-rgb), 0.15);
}
.flare {
  position: absolute;
  left: calc(50% - 150px);
  top: calc(100% - var(--horizon) - 7px);
  width: 300px;
  height: 14px;
  border-radius: 50%;
  background: radial-gradient(ellipse, rgba(190, 255, 225, 0.9), transparent 70%);
}
</style>
