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
  box-sizing: border-box; min-width: 0; min-height: 87px; padding: 14px 30px 14px 20px;
  border-radius: var(--r-22); border: 1px solid var(--line); background: var(--card-bg);
  color: inherit; font: inherit; font-weight: 400; text-align: left; cursor: pointer;
  transition: border-color 0.2s ease, box-shadow 0.2s ease;
}
.row { display: flex; align-items: center; gap: 22px; min-width: 0; }
.body { display: block; min-width: 0; }
.jv-card:hover { border-color: var(--line-a45); box-shadow: 0 0 22px rgba(var(--accent-rgb), 0.12); }
.icon {
  width: 58px; height: 58px; flex: none; box-sizing: border-box; display: grid; place-items: center;
  border-radius: 50%; border: 1.5px solid var(--line-a55); color: var(--accent);
}
.icon :deep(svg) { width: 26px; height: 26px; stroke-width: 1.5; }
.text { display: flex; flex-direction: column; min-width: 0; }
.title { font-size: var(--fs-12); font-weight: 600; letter-spacing: 0.1em; color: var(--text-0); }
.line { font-size: var(--fs-11); letter-spacing: 0.06em; color: var(--text-5); line-height: 1.5; white-space: nowrap; overflow: hidden; text-overflow: ellipsis; }
.title + .line { margin-top: 5px; }
.value {
  margin-top: 4px; font-family: var(--font-display); font-size: 26px; font-weight: 600; line-height: 1.1;
  letter-spacing: 0.04em; color: var(--text-0); font-variant-numeric: tabular-nums;
  white-space: nowrap; overflow: hidden; text-overflow: ellipsis;
}
.value + .line { margin-top: 2px; }
.dot { position: absolute; top: 16px; right: 18px; width: 5px; height: 5px; border-radius: 50%; background: var(--idle); }
.dot.ok { background: var(--accent); box-shadow: var(--glow-sm); }
.dot.warn { background: var(--warn); box-shadow: 0 0 8px var(--warn); }
.dot.error { background: var(--danger); box-shadow: 0 0 8px var(--danger); }
</style>
