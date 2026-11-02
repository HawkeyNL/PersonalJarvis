<script setup lang="ts">
import StatusBadge from "../StatusBadge.vue";
import type { NeedItem } from "./needs";

// Rows that need the owner, each with one action. A skeleton row stands in
// while any source is still loading; "All good" only once every source read.
// One persistent live region announces the outcome once loading ends.
defineProps<{ items: NeedItem[]; loading: boolean; busy: boolean }>();
defineEmits<{ act: [target: NeedItem["target"], service?: NeedItem["service"]] }>();
</script>

<template>
  <section class="needs" aria-labelledby="needs-title">
    <h2 id="needs-title">Needs you</h2>
    <p class="sr-only" role="status">{{ loading ? "" : items.length ? `${items.length} item${items.length === 1 ? " needs" : "s need"} you.` : "All good." }}</p>
    <ul v-if="items.length || loading" class="needs-list">
      <li v-for="item in items" :key="item.key" class="needs-row">
        <StatusBadge :tone="item.tone" :label="item.tone === 'error' ? 'Problem' : 'Action'" />
        <span class="needs-text"><strong>{{ item.title }}</strong><span>{{ item.detail }}</span></span>
        <button type="button" class="small secondary" :aria-label="`${item.action}: ${item.title}`" :disabled="busy && item.target === 'refresh'" @click="$emit('act', item.target, item.service)">{{ item.action }}</button>
      </li>
      <li v-if="loading" class="needs-row skeleton" aria-busy="true"><span class="sr-only">Checking the Home Node…</span><i aria-hidden="true" /><i aria-hidden="true" /></li>
    </ul>
    <p v-else class="needs-calm"><StatusBadge tone="ok" label="All good" />Nothing needs you right now.</p>
  </section>
</template>

<style scoped>
.needs { margin: 0 0 28px; }
.needs h2 { margin: 0 0 12px; font-size: 15px; }
.needs-list { display: grid; gap: 8px; margin: 0; padding: 0; list-style: none; }
.needs-row {
  display: flex; align-items: center; gap: 14px; min-height: 56px; padding: 10px 16px;
  border-radius: var(--r-md); background: var(--bg-surface); box-shadow: var(--shadow-border);
}
.needs-text { display: flex; flex: 1; min-width: 0; flex-direction: column; gap: 2px; font-size: 13px; color: var(--text-secondary); overflow-wrap: anywhere; }
.needs-text strong { color: var(--text-primary); font-size: 14px; font-weight: 500; }
.needs-row > .badge { min-width: 80px; justify-content: center; }
.needs-row > button { flex-shrink: 0; }
.skeleton { gap: 16px; }
.skeleton i { display: block; height: 12px; border-radius: var(--r-pill); background: var(--bg-sunken); }
.skeleton i:first-of-type { width: 72px; }
.skeleton i:last-of-type { width: min(320px, 50%); }
.needs-calm { display: flex; align-items: center; gap: 12px; min-height: 56px; margin: 0; padding: 10px 16px; border-radius: var(--r-md); background: var(--bg-surface); box-shadow: var(--shadow-border); color: var(--text-secondary); font-size: 13px; }
</style>
