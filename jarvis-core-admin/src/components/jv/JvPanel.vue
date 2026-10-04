<script setup lang="ts">
import NavIcon, { type IconName } from "../NavIcon.vue";
import JvStatusDot from "./JvStatusDot.vue";
import type { Tone } from "../../admin";

// Detail panel of a node page: header (icon, title, status, description,
// italic detail line, optional action) and the body slot below it.
defineProps<{ icon: IconName; title: string; tone: Tone; status: string; description?: string; quote?: string }>();
</script>

<template>
  <section class="jv-panel" :aria-label="title">
    <header class="head">
      <span class="icon" aria-hidden="true"><NavIcon :name="icon" /></span>
      <div class="meta">
        <div class="line1">
          <h2>{{ title }}</h2>
          <JvStatusDot :tone="tone" :label="status" :size="10" />
        </div>
        <p v-if="description" class="desc">{{ description }}</p>
        <p v-if="quote" class="quote">{{ quote }}</p>
      </div>
      <div v-if="$slots.action" class="action"><slot name="action" /></div>
    </header>
    <slot />
  </section>
</template>

<style scoped>
.jv-panel {
  box-sizing: border-box; min-width: 0;
  padding: 15px 31px 23px; border-radius: var(--r-26);
  border: 1.5px solid var(--line-a55); background: var(--panel-bg);
  box-shadow: 0 0 30px rgba(var(--accent-rgb), 0.18), inset 0 0 40px rgba(var(--accent-rgb), 0.05);
  display: flex; flex-direction: column; gap: 15px;
}
.head { display: flex; align-items: center; gap: 24px; min-height: 80px; }
.icon {
  width: 78px; height: 78px; flex: none; box-sizing: border-box; display: grid; place-items: center;
  border-radius: 50%; border: 1.5px solid rgba(var(--accent-rgb), 0.75); color: var(--accent);
  box-shadow: inset 0 0 16px rgba(var(--accent-rgb), 0.22);
}
.icon :deep(svg) { width: 34px; height: 34px; stroke-width: 1.5; }
.meta { flex: 1; min-width: 0; }
.line1 { display: flex; align-items: center; gap: 14px; flex-wrap: wrap; }
h2 { margin: 0; font-size: var(--fs-20); font-weight: 500; letter-spacing: 0.04em; color: #f8fffb; }
.line1 :deep(.jv-status) { font-size: var(--fs-13); }
.desc { margin: 6px 0 0; font-size: var(--fs-14); color: var(--text-2); }
.quote { margin: 5px 0 0; font-size: var(--fs-12); font-style: italic; font-weight: 300; color: #9fb8ad; overflow-wrap: anywhere; }
.action { flex: none; align-self: flex-start; margin-top: 20px; }
@media (max-width: 1099px), (max-height: 759px) {
  .jv-panel { padding: 14px 16px 18px; border-radius: var(--r-22); }
  .head { gap: 14px; flex-wrap: wrap; }
  .icon { width: 56px; height: 56px; }
  .icon :deep(svg) { width: 26px; height: 26px; }
  .action { margin-top: 0; }
}
</style>
