<script setup lang="ts">
import { computed } from "vue";
import { healthy } from "../admin";
const props = defineProps<{ state: string }>();
const tone = computed(() => {
  const state = props.state.toLowerCase();
  if (healthy(state) || state === "yes" || state === "available") return "ok";
  if (["failed", "error", "inactive", "disabled", "unavailable"].some((v) => state.includes(v))) return "bad";
  return "warn";
});
</script>

<template><span class="status-badge" :class="tone"><i />{{ state }}</span></template>
