<script setup lang="ts">
import type { OperationResult } from "../admin";
import StatusBadge from "./StatusBadge.vue";
defineProps<{ result: OperationResult }>();
</script>
<template>
  <section class="result-panel" role="status">
    <p class="result-summary"><StatusBadge :tone="result.success ? 'ok' : 'error'" :label="result.success ? 'Done' : 'Failed'" />{{ result.summary }}</p>
    <details :open="!result.success"><summary>Technical output</summary><pre>{{ result.detail }}</pre></details>
  </section>
</template>
