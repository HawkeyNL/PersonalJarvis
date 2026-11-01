<script setup lang="ts">
import NavIcon, { type IconName } from "../NavIcon.vue";

// One vital in a panel's tile grid: icon, label, value.
defineProps<{ icon: IconName; label: string; value: string }>();
</script>

<template>
  <div class="jv-tile">
    <NavIcon :name="icon" class="icon" />
    <div class="text">
      <div class="label">{{ label }}</div>
      <div class="value">{{ value }}</div>
    </div>
  </div>
</template>

<style scoped>
.jv-tile {
  min-height: 68px; min-width: 0; display: flex; align-items: center; gap: 12px;
  padding: 12px 14px; border-radius: var(--r-lg); background: var(--bg-surface); box-shadow: var(--shadow-border);
}
.icon { flex: none; width: 36px; height: 36px; padding: 8px; border-radius: 10px; background: var(--bg-sunken); color: var(--text-secondary); stroke-width: 1.6; }
.text { min-width: 0; }
.label { font-size: 13px; font-weight: 500; color: var(--text-primary); line-height: 1.3; }
.value { margin-top: 2px; font-size: 13px; color: var(--text-secondary); line-height: 1.4; overflow-wrap: anywhere; }
</style>
