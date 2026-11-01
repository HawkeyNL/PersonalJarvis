<script setup lang="ts">withDefaults(defineProps<{ message: string; title?: string }>(), { title: "Operation could not be completed" });</script>
<template>
  <div class="error-panel" role="alert">
    <strong>{{ title }}</strong>
    <p>{{ message }}</p>
  </div>
</template>
