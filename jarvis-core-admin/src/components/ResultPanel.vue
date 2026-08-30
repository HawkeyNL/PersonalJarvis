<script setup lang="ts">
import type { OperationResult } from "../admin";
defineProps<{ result: OperationResult }>();
</script>
<template>
  <section class="result-panel" :class="result.success ? 'success' : 'failure'">
    <strong>{{ result.success ? "✓" : "✕" }} {{ result.summary }}</strong>
    <pre>{{ result.detail }}</pre>
  </section>
</template>
