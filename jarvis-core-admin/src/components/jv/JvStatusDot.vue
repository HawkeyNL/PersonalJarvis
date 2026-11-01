<script setup lang="ts">
import type { Tone } from "../../admin";

// A status badge: the label carries the meaning, the dot is decoration.
// Without a label only the dot renders.
withDefaults(defineProps<{ tone: Tone; label?: string; size?: number }>(), { size: 6 });
</script>

<template>
  <span class="jv-status" :class="[tone, { badge: label }]">
    <span class="dot" :style="{ width: size + 'px', height: size + 'px' }" aria-hidden="true"></span>
    <span v-if="label" class="label">{{ label }}</span>
  </span>
</template>

<style scoped>
.jv-status { display: inline-flex; align-items: center; gap: 6px; color: var(--text-secondary); }
.jv-status.badge { color: var(--text-primary); }
.dot { flex: none; border-radius: 50%; background: currentColor; }
.ok.badge { background: var(--positive-subtle); color: var(--positive-text); }
.warn.badge { background: var(--negative-subtle); color: var(--negative-text); }
.error.badge { background: var(--danger-subtle); color: var(--danger-text); }
.ok:not(.badge) { color: var(--positive-solid); }
.warn:not(.badge) { color: var(--negative-solid); }
.error:not(.badge) { color: var(--danger-solid); }
</style>
