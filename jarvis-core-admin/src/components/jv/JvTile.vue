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
  box-sizing: border-box; min-height: 69px; min-width: 0; display: flex; align-items: center; gap: 12px;
  padding: 8px 12px; border-radius: var(--r-12); border: 1px solid var(--line-a30); background: var(--tile-bg);
}
.icon { flex: none; width: 30px; height: 30px; color: var(--accent); stroke-width: 1.5; }
.text { min-width: 0; }
.label { font-size: var(--fs-12); font-weight: 500; color: #f1faf6; line-height: 1.3; }
.value { margin-top: 3px; font-size: 11px; color: var(--text-5); line-height: 1.3; overflow-wrap: anywhere; }
</style>
