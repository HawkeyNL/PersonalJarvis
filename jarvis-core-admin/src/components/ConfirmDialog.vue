<script setup lang="ts">
import { ref } from "vue";
defineProps<{ title: string; detail: string; confirmLabel: string; requireText?: string }>();
defineEmits<{ cancel: []; confirm: [] }>();
const typed = ref("");
</script>
<template>
  <div class="dialog-backdrop" role="presentation" @click.self="$emit('cancel')">
    <section class="dialog" role="dialog" aria-modal="true" :aria-label="title">
      <p class="eyebrow">OWNER CONFIRMATION</p>
      <h2>{{ title }}</h2>
      <p>{{ detail }}</p>
      <label v-if="requireText">Type <strong>{{ requireText }}</strong> to confirm
        <input v-model="typed" autocomplete="off" spellcheck="false" :aria-label="`Type ${requireText} to confirm`" />
      </label>
      <div class="dialog-actions">
        <button class="secondary" autofocus @click="$emit('cancel')">Cancel</button>
        <button class="danger" :disabled="!!requireText && typed !== requireText" @click="$emit('confirm')">{{ confirmLabel }}</button>
      </div>
    </section>
  </div>
</template>
