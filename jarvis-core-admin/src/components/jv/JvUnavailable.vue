<script setup lang="ts">
import NavIcon, { type IconName } from "../NavIcon.vue";

// Honest empty state for features the Core does not offer (yet). Never shows
// sample data. `kind` picks the copy: a feature that does not exist yet, or a
// Core that is too old for an existing feature.
withDefaults(defineProps<{ title: string; kind?: "planned" | "core-update" | "error"; detail?: string; icon?: IconName }>(), {
  kind: "planned",
  icon: "spark",
});
</script>

<template>
  <div class="jv-unavailable" role="status">
    <span class="icon" aria-hidden="true"><NavIcon :name="icon" /></span>
    <div>
      <p class="title">{{ title }}</p>
      <p class="kind">
        {{ kind === "core-update" ? "Requires newer Core" : kind === "error" ? "Could not load" : "Not yet available" }}
      </p>
      <p v-if="detail" class="detail">{{ detail }}</p>
    </div>
  </div>
</template>

<style scoped>
.jv-unavailable {
  display: flex; align-items: flex-start; gap: 16px; padding: 18px 20px; box-sizing: border-box;
  border: 1px dashed var(--line-a30); border-radius: var(--r-12); background: rgba(8, 40, 32, 0.3);
}
.icon {
  flex: none; width: 40px; height: 40px; display: grid; place-items: center; border-radius: 50%;
  border: 1.5px solid var(--line-a30); color: var(--text-5);
}
.icon :deep(svg) { width: 20px; height: 20px; }
p { margin: 0; }
.title { font-size: var(--fs-13); font-weight: 500; color: var(--text-1); }
.kind { margin-top: 3px; font-size: 11px; letter-spacing: 0.2em; text-transform: uppercase; color: var(--text-5); }
.detail { margin-top: 8px; font-size: var(--fs-12); color: var(--text-4); line-height: 1.5; max-width: 60ch; }
</style>
