<script setup lang="ts">
defineProps<{ title: string; detail: string; confirmLabel: string }>();
defineEmits<{ cancel: []; confirm: [] }>();
</script>
<template>
  <div class="dialog-backdrop" role="presentation" @click.self="$emit('cancel')">
    <section class="dialog" role="dialog" aria-modal="true" :aria-label="title">
      <p class="eyebrow">OWNER CONFIRMATION</p>
      <h2>{{ title }}</h2>
      <p>{{ detail }}</p>
      <div class="dialog-actions">
        <button class="secondary" autofocus @click="$emit('cancel')">Cancel</button>
        <button class="danger" @click="$emit('confirm')">{{ confirmLabel }}</button>
      </div>
    </section>
  </div>
</template>
