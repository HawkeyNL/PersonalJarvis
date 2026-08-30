<script setup lang="ts">defineProps<{ message: string }>();</script>
<template>
  <div class="error-panel" role="alert">
    <strong>Operation could not be completed</strong>
    <p>{{ message }}</p>
  </div>
</template>
