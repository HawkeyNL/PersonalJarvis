<script setup lang="ts">
import { onMounted } from "vue";
import {
  biometricUnlock,
  unlocking,
  lockError,
  phoneWaiting,
  phoneError,
  requestPhoneApproval,
  cancelPhoneApproval,
} from "../lock";

// Offer Touch ID immediately. If it fails or the machine has no biometric
// hardware (e.g. a Mac without Touch ID), fall straight through to the phone
// route rather than the desktop password.
onMounted(async () => {
  const ok = await biometricUnlock();
  if (!ok) requestPhoneApproval();
});
</script>

<template>
  <div class="lock">
    <div class="card">
      <div class="mark">
        <span class="ring"></span>
        <span class="glyph">J</span>
      </div>
      <h1>Jarvis is vergrendeld</h1>
      <p class="sub">Verifieer jezelf om verder te gaan.</p>

      <button class="primary" :disabled="unlocking || phoneWaiting" @click="biometricUnlock">
        {{ unlocking ? "Even verifiëren…" : "Ontgrendel met Touch ID / Face ID" }}
      </button>

      <template v-if="!phoneWaiting">
        <button class="ghost" @click="requestPhoneApproval">Ontgrendel via telefoon</button>
      </template>
      <div v-else class="waiting">
        <span class="spinner" aria-hidden="true"></span>
        Wachten op goedkeuring via je telefoon…
        <button class="link" @click="cancelPhoneApproval">annuleren</button>
      </div>

      <p v-if="phoneError || lockError" class="err">{{ phoneError || lockError }}</p>
    </div>
  </div>
</template>

<style scoped>
.lock {
  position: fixed;
  inset: 0;
  z-index: 200;
  display: grid;
  place-items: center;
  padding: 24px;
  background: radial-gradient(ellipse at 50% 40%, rgba(8, 20, 14, 0.86), rgba(3, 7, 5, 0.96));
  backdrop-filter: blur(26px) saturate(1.2);
  -webkit-backdrop-filter: blur(26px) saturate(1.2);
}
.card {
  width: min(360px, 92vw);
  display: flex;
  flex-direction: column;
  align-items: center;
  text-align: center;
  gap: 12px;
}
.mark {
  position: relative;
  width: 78px;
  height: 78px;
  display: grid;
  place-items: center;
  margin-bottom: 4px;
}
.mark .ring {
  position: absolute;
  inset: 0;
  border-radius: 50%;
  border: 1.5px solid var(--accent);
  box-shadow: 0 0 26px rgba(52, 245, 160, 0.4), inset 0 0 18px rgba(52, 245, 160, 0.25);
  animation: pulse 2.6s ease-in-out infinite;
}
.mark .glyph {
  font-family: var(--mono);
  font-size: 30px;
  font-weight: 700;
  color: var(--accent);
  text-shadow: 0 0 14px rgba(52, 245, 160, 0.6);
}
h1 {
  font-size: 19px;
  margin: 0;
  letter-spacing: 0.02em;
}
.sub {
  margin: 0 0 6px;
  color: var(--muted);
  font-size: 13px;
}

.primary,
.ghost {
  width: 100%;
  border-radius: 12px;
  padding: 12px 16px;
  font: inherit;
  font-size: 14px;
  font-weight: 600;
  cursor: pointer;
}
.primary {
  background: linear-gradient(180deg, var(--accent), var(--accent-2));
  color: #04140c;
  border: none;
}
.primary:disabled {
  opacity: 0.6;
  cursor: default;
}
.ghost {
  background: transparent;
  border: 1px solid var(--border);
  color: var(--text);
  font-weight: 500;
}
.ghost:hover {
  border-color: var(--accent);
}

.waiting {
  display: flex;
  align-items: center;
  justify-content: center;
  gap: 9px;
  flex-wrap: wrap;
  color: var(--accent);
  font-size: 13px;
}
.spinner {
  width: 15px;
  height: 15px;
  border-radius: 50%;
  border: 2px solid rgba(52, 245, 160, 0.3);
  border-top-color: var(--accent);
  animation: spin 0.9s linear infinite;
}
.link {
  background: none;
  border: none;
  color: var(--muted);
  text-decoration: underline;
  cursor: pointer;
  font: inherit;
  font-size: 12px;
}
.err {
  color: #f87171;
  font-size: 12px;
  margin: 4px 0 0;
}

@keyframes spin {
  to { transform: rotate(360deg); }
}
@keyframes pulse {
  0%, 100% { box-shadow: 0 0 18px rgba(52, 245, 160, 0.28), inset 0 0 14px rgba(52, 245, 160, 0.2); }
  50% { box-shadow: 0 0 34px rgba(52, 245, 160, 0.55), inset 0 0 22px rgba(52, 245, 160, 0.32); }
}
@media (prefers-reduced-motion: reduce) {
  .mark .ring,
  .spinner {
    animation: none;
  }
}
</style>
