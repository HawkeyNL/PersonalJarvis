<script setup lang="ts">
defineProps<{ title: string; description: string; busy?: boolean }>();
</script>

<template>
  <header class="page-header">
    <div>
      <p class="eyebrow">HOME NODE</p>
      <h1>{{ title }}</h1>
      <p>{{ description }}</p>
    </div>
    <span v-if="busy" class="busy"><span class="spinner" /> Working</span>
    <div v-else class="page-actions"><slot /></div>
  </header>
</template>
