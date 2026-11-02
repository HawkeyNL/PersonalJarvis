<script setup lang="ts">
import { computed, onBeforeUnmount, onMounted, ref, useId } from "vue";
import type { PendingDevice } from "../../admin";
import { dialogFocus } from "../../focus";
import { countdown, groupFingerprint, isExpired } from "./pairing";
// Approve and Deny in one dialog. Like ConfirmDialog it stays inside
// .app-shell (no Teleport), so the shell's activity listeners keep the idle
// lock working. Initial focus: Cancel, so Enter never approves by accident.
const props = defineProps<{ request: PendingDevice; now: number }>();
const emit = defineEmits<{ cancel: []; approve: []; deny: [] }>();
const expired = computed(() => isExpired(props.request.expires_at, props.now));
const id = useId();
const dialog = ref<HTMLElement | null>(null);
let focus: ReturnType<typeof dialogFocus> | undefined;
onMounted(() => {
  focus = dialogFocus(dialog.value!, () => emit("cancel"));
  dialog.value!.querySelector<HTMLElement>("button")?.focus();
});
onBeforeUnmount(() => focus?.restore());
</script>
<template>
  <div class="dialog-backdrop" role="presentation" @click.self="$emit('cancel')">
    <section ref="dialog" class="dialog" role="dialog" aria-modal="true" tabindex="-1" :aria-labelledby="`${id}-title`" :aria-describedby="`${id}-detail`" @keydown="focus?.keydown($event)">
      <h2 :id="`${id}-title`">Review {{ request.name }}</h2>
      <p :id="`${id}-detail`">{{ request.platform }} · {{ expired ? "This request expired" : `Expires in ${countdown(request.expires_at, now)}` }}. Name and platform come from the new device and are not verified.</p>
      <div class="well">
        <span class="card-label">Key fingerprint</span>
        <p class="fingerprint">{{ groupFingerprint(request.fingerprint) }}</p>
      </div>
      <p>Compare it with the fingerprint the new device shows. Approve only if every group matches. The system asks for the administrator password next.</p>
      <div class="dialog-actions">
        <button class="secondary" @click="$emit('cancel')">Cancel</button>
        <button class="secondary" :disabled="expired" @click="$emit('deny')">Deny</button>
        <button class="primary" :disabled="expired" @click="$emit('approve')">Approve on this Home Node</button>
      </div>
    </section>
  </div>
</template>
<style scoped>
.well { margin-top: 16px; }
.fingerprint { margin: 6px 0 0; color: var(--text-primary); font: 15px/1.6 var(--font-mono); letter-spacing: 0.02em; word-spacing: 0.2em; }
</style>
