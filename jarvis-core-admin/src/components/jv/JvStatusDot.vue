<script setup lang="ts">
import type { Tone } from "../../admin";

// A status dot is decoration; the label carries the meaning.
withDefaults(defineProps<{ tone: Tone; label?: string; size?: number }>(), { size: 9 });
</script>

<template>
  <span class="jv-status" :class="tone">
    <span class="dot" :style="{ width: size + 'px', height: size + 'px' }" aria-hidden="true"></span>
    <span v-if="label" class="label">{{ label }}</span>
  </span>
</template>

<style scoped>
.jv-status { display: inline-flex; align-items: center; gap: 8px; }
.dot { flex: none; border-radius: 50%; background: var(--idle); }
.ok .dot { background: var(--accent); box-shadow: var(--glow-sm); }
.warn .dot { background: var(--warn); box-shadow: 0 0 8px rgba(245, 184, 74, 0.6); }
.error .dot { background: var(--danger); box-shadow: 0 0 8px rgba(248, 113, 113, 0.6); }
.ok .label { color: var(--accent); }
.idle .label { color: #9aa7ad; }
.warn .label { color: var(--warn); }
.error .label { color: var(--danger); }
</style>
