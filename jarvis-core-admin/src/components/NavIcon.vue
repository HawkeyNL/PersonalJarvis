<script setup lang="ts">
defineProps<{ name: string }>();
</script>

<template>
  <svg viewBox="0 0 24 24" aria-hidden="true">
    <path v-if="name === 'overview'" d="M4 13h7V4H4v9Zm0 7h7v-4H4v4Zm10 0h6v-9h-6v9Zm0-16v4h6V4h-6Z" />
    <path v-else-if="name === 'health'" d="M3 12h4l2.3-7 4.2 14 2.2-7H21" />
    <path v-else-if="name === 'services'" d="M5 5h14v5H5V5Zm0 9h14v5H5v-5Zm3-6h.01M8 17h.01" />
    <path v-else-if="name === 'update'" d="M12 4v11m0 0-4-4m4 4 4-4M5 19h14" />
    <path v-else-if="name === 'agents'" d="M8 8a4 4 0 1 0 8 0 4 4 0 0 0-8 0Zm-3 12a7 7 0 0 1 14 0M4 5h2m12 0h2" />
    <path v-else-if="name === 'models'" d="M12 3 4 7v10l8 4 8-4V7l-8-4Zm-8 4 8 4 8-4m-8 4v10" />
    <path v-else-if="name === 'credentials'" d="M8 11V8a4 4 0 0 1 8 0v3m-9 0h10v9H7v-9Zm5 4v2" />
    <path v-else-if="name === 'logs'" d="M5 4h14v16H5V4Zm3 4h8m-8 4h8m-8 4h5" />
    <path v-else d="M12 3v3m0 12v3M3 12h3m12 0h3M5.6 5.6l2.1 2.1m8.6 8.6 2.1 2.1m0-12.8-2.1 2.1m-8.6 8.6-2.1 2.1M12 9a3 3 0 1 0 0 6 3 3 0 0 0 0-6Z" />
  </svg>
</template>
