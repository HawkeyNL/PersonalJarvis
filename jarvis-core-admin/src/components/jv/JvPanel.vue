<script setup lang="ts">
import NavIcon, { type IconName } from "../NavIcon.vue";
import StatusBadge from "../StatusBadge.vue";
import type { Tone } from "../../status";

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
          <StatusBadge :tone="tone" :label="status" />
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
  padding: 20px 24px 24px; border-radius: var(--r-lg);
  background: var(--bg-surface); box-shadow: var(--shadow-border);
  display: flex; flex-direction: column; gap: 16px;
}
.head { display: flex; align-items: center; gap: 16px; }
.icon {
  width: 44px; height: 44px; flex: none; display: grid; place-items: center;
  border-radius: var(--r-md); background: var(--accent-subtle); color: var(--accent-text);
}
.icon :deep(svg) { width: 22px; height: 22px; stroke-width: 1.6; }
.meta { flex: 1; min-width: 0; }
.line1 { display: flex; align-items: center; gap: 12px; flex-wrap: wrap; }
h2 { margin: 0; font-size: 18px; line-height: 1.3; font-weight: 600; letter-spacing: -0.01em; color: var(--text-primary); }
.desc { margin: 4px 0 0; font-size: 14px; color: var(--text-secondary); }
.quote { margin: 4px 0 0; font-size: 13px; color: var(--text-secondary); overflow-wrap: anywhere; }
.action { flex: none; align-self: flex-start; }
@media (max-width: 1099px), (max-height: 759px) {
  .jv-panel { padding: 16px 16px 18px; }
  .head { flex-wrap: wrap; }
}
</style>
