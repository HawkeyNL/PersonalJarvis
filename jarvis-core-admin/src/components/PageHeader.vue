<script setup lang="ts">
import { inject } from "vue";
defineProps<{ title: string; description: string; busy?: boolean }>();
// A view embedded in another page (Health and Services on Overview) gets an h2.
const level = inject("pageHeadingLevel", 1);
</script>

<template>
  <header class="page-header">
    <div>
      <h1 v-if="level === 1">{{ title }}</h1>
      <h2 v-else>{{ title }}</h2>
      <p>{{ description }}</p>
    </div>
    <span v-if="busy" class="busy"><span class="spinner" /> Working</span>
    <div v-else class="page-actions"><slot /></div>
  </header>
</template>
