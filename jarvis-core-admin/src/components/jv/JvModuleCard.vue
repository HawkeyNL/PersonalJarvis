<script setup lang="ts">
import { computed } from "vue";
import NavIcon, { type IconName } from "../NavIcon.vue";
import type { Tone } from "../../admin";

// A dashboard card: a button with icon, name, an optional headline value,
// lines of live summary and an optional body (a meter or chart) below.
// Clicking opens the detail view.
const props = defineProps<{ icon: IconName; title: string; value?: string; lines: string[]; tone: Tone }>();
defineEmits<{ open: [] }>();
const accessibleName = computed(() => [props.title, props.value, ...props.lines].filter(Boolean).join(", "));
</script>

<template>
  <button type="button" class="jv-card" :aria-label="accessibleName" @click="$emit('open')">
    <span class="row">
      <span class="icon" aria-hidden="true"><NavIcon :name="icon" /></span>
      <span class="text" aria-hidden="true">
        <span class="title">{{ title }}</span>
        <span v-if="value" class="value">{{ value }}</span>
        <span v-for="(line, i) in lines" :key="i" class="line">{{ line }}</span>
      </span>
    </span>
    <span v-if="$slots.default" class="body"><slot /></span>
    <span class="dot" :class="tone" aria-hidden="true"></span>
  </button>
</template>

<style scoped>
.jv-card {
  position: relative; display: flex; flex-direction: column; align-items: stretch; justify-content: flex-start; gap: 14px;
  min-width: 0; min-height: 88px; padding: 16px 28px 16px 18px;
  border-radius: var(--r-lg); background: var(--bg-surface); box-shadow: var(--shadow-border);
  color: inherit; font: inherit; font-weight: 400; line-height: 1.5; text-align: left; white-space: normal;
}
.row { display: flex; align-items: center; gap: 14px; min-width: 0; }
.body { display: block; min-width: 0; }
@media (hover: hover) and (pointer: fine) {
  .jv-card:hover { background: var(--bg-surface); box-shadow: var(--shadow-border-hover); }
}
@media (prefers-reduced-motion: no-preference) {
  .jv-card:active { transform: scale(0.99); }
}
.icon {
  width: 36px; height: 36px; flex: none; display: grid; place-items: center;
  border-radius: 10px; background: var(--bg-sunken); color: var(--text-secondary);
}
.icon :deep(svg) { width: 20px; height: 20px; stroke-width: 1.6; }
.text { display: flex; flex-direction: column; min-width: 0; }
.title { font-size: 13px; font-weight: 500; color: var(--text-secondary); }
.line { font-size: 13px; color: var(--text-secondary); line-height: 1.45; white-space: nowrap; overflow: hidden; text-overflow: ellipsis; }
.title + .line { margin-top: 4px; }
.value {
  margin-top: 2px; font-size: 22px; font-weight: 600; line-height: 1.2; letter-spacing: -0.01em;
  color: var(--text-primary); font-variant-numeric: tabular-nums;
  white-space: nowrap; overflow: hidden; text-overflow: ellipsis;
}
.value + .line { margin-top: 2px; }
.dot { position: absolute; top: 16px; right: 16px; width: 6px; height: 6px; border-radius: 50%; background: var(--border-strong); }
.dot.ok { background: var(--positive-solid); }
.dot.warn { background: var(--negative-solid); }
.dot.error { background: var(--danger-solid); }
</style>
