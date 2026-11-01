<script setup lang="ts">
import NavIcon, { type IconName } from "../NavIcon.vue";

// Honest empty state for data the Core does not report (yet). Never shows
// sample data: a dimension Core does not measure yet, or an older Core omits.
withDefaults(defineProps<{ title: string; detail?: string; icon?: IconName }>(), {
  icon: "spark",
});
</script>

<template>
  <div class="jv-unavailable" role="status">
    <span class="icon" aria-hidden="true"><NavIcon :name="icon" /></span>
    <div>
      <p class="title">{{ title }}</p>
      <p class="kind">Not measured yet</p>
      <p v-if="detail" class="detail">{{ detail }}</p>
    </div>
  </div>
</template>

<style scoped>
.jv-unavailable {
  display: flex; align-items: flex-start; gap: 14px; padding: 16px 18px;
  border-radius: var(--r-md); background: var(--bg-sunken);
}
.icon {
  flex: none; width: 36px; height: 36px; display: grid; place-items: center; border-radius: 10px;
  background: var(--bg-surface); color: var(--text-secondary);
}
.icon :deep(svg) { width: 20px; height: 20px; }
p { margin: 0; }
.title { font-size: 14px; font-weight: 600; color: var(--text-primary); }
.kind { margin-top: 2px; font-size: 13px; color: var(--text-secondary); }
.detail { margin-top: 8px; font-size: 13px; color: var(--text-secondary); line-height: 1.5; max-width: 60ch; }
</style>
