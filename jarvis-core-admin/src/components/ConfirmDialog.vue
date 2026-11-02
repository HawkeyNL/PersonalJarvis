<script setup lang="ts">
import { onBeforeUnmount, onMounted, ref, useId } from "vue";
import { dialogFocus } from "../focus";
// `tone` styles the confirm button: danger only for destructive actions.
const props = defineProps<{ title: string; detail: string; confirmLabel: string; tone: "neutral" | "primary" | "danger"; requireText?: string; cancelLabel?: string }>();
const emit = defineEmits<{ cancel: []; confirm: [] }>();
const typed = ref("");
const id = useId();
const dialog = ref<HTMLElement | null>(null);
let focus: ReturnType<typeof dialogFocus> | undefined;
// Stays inside .app-shell (no Teleport), so the shell's activity listeners keep
// the idle lock working. Initial focus: the confirmation text field, else Cancel.
onMounted(() => {
  focus = dialogFocus(dialog.value!, () => emit("cancel"));
  dialog.value!.querySelector<HTMLElement>(props.requireText ? "input" : "button")?.focus();
});
onBeforeUnmount(() => focus?.restore());
</script>
<template>
  <div class="dialog-backdrop" role="presentation" @click.self="$emit('cancel')">
    <section ref="dialog" class="dialog" role="dialog" aria-modal="true" tabindex="-1" :aria-labelledby="`${id}-title`" :aria-describedby="`${id}-detail`" @keydown="focus?.keydown($event)">
      <h2 :id="`${id}-title`">{{ title }}</h2>
      <p :id="`${id}-detail`">{{ detail }}</p>
      <label v-if="requireText">Type <strong>{{ requireText }}</strong> to confirm
        <input v-model="typed" autocomplete="off" spellcheck="false" :aria-label="`Type ${requireText} to confirm`" />
      </label>
      <div class="dialog-actions">
        <button class="secondary" @click="$emit('cancel')">{{ cancelLabel ?? "Cancel" }}</button>
        <button :class="tone === 'neutral' ? 'secondary' : tone" :disabled="!!requireText && typed !== requireText" @click="$emit('confirm')">{{ confirmLabel }}</button>
      </div>
    </section>
  </div>
</template>
