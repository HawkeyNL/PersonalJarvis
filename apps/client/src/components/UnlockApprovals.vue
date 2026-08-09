<script setup lang="ts">
import NavIcon from "./NavIcon.vue";
import { pending, approve, approving, approvalError } from "../unlockApprovals";
</script>

<template>
  <Transition name="sheet">
    <div v-if="pending.length" class="wrap">
      <div class="sheet">
        <div class="lead"><NavIcon name="link" /> Ontgrendelverzoek</div>
        <div v-for="r in pending" :key="r.id" class="req">
          <div class="who">
            <strong>{{ r.device_name }}</strong>
            <span class="plat">{{ r.platform }}</span>
            wil ontgrendelen
          </div>
          <button class="approve" :disabled="approving === r.id" @click="approve(r)">
            {{ approving === r.id ? "Verifiëren…" : "Goedkeuren" }}
          </button>
        </div>
        <p v-if="approvalError" class="err">{{ approvalError }}</p>
        <p class="hint">Bevestig met Face ID / Touch ID op dit toestel.</p>
      </div>
    </div>
  </Transition>
</template>

<style scoped>
.wrap {
  position: fixed;
  left: 0;
  right: 0;
  bottom: calc(96px + env(safe-area-inset-bottom));
  z-index: 180;
  display: flex;
  justify-content: center;
  padding: 0 14px;
  pointer-events: none;
}
.sheet {
  width: min(440px, 100%);
  pointer-events: auto;
  border-radius: 18px;
  padding: 14px 16px 12px;
  background: linear-gradient(180deg, rgba(16, 34, 26, 0.86), rgba(8, 18, 14, 0.82));
  border: 1px solid var(--accent);
  box-shadow: 0 18px 50px rgba(0, 0, 0, 0.55), 0 0 22px rgba(52, 245, 160, 0.22);
  backdrop-filter: blur(22px) saturate(1.3);
  -webkit-backdrop-filter: blur(22px) saturate(1.3);
}
.lead {
  display: flex;
  align-items: center;
  gap: 8px;
  font-family: var(--mono);
  font-size: 11px;
  letter-spacing: 0.16em;
  color: var(--accent);
  margin-bottom: 10px;
}
.lead :deep(svg) {
  width: 15px;
  height: 15px;
}
.req {
  display: flex;
  align-items: center;
  gap: 12px;
  padding: 6px 0;
}
.who {
  font-size: 14px;
  color: var(--text);
}
.who .plat {
  font-family: var(--mono);
  font-size: 10px;
  color: var(--muted);
  margin: 0 4px;
}
.approve {
  margin-left: auto;
  flex: none;
  background: linear-gradient(180deg, var(--accent), var(--accent-2));
  color: #04140c;
  border: none;
  border-radius: 11px;
  padding: 9px 16px;
  font: inherit;
  font-size: 13px;
  font-weight: 700;
  cursor: pointer;
}
.approve:disabled {
  opacity: 0.6;
  cursor: default;
}
.err {
  color: #f87171;
  font-size: 12px;
  margin: 6px 0 0;
}
.hint {
  color: var(--muted);
  font-size: 11px;
  margin: 8px 0 0;
}

.sheet-enter-active,
.sheet-leave-active {
  transition: transform 0.3s cubic-bezier(0.22, 1, 0.36, 1), opacity 0.3s ease;
}
.sheet-enter-from,
.sheet-leave-to {
  transform: translateY(16px);
  opacity: 0;
}
@media (prefers-reduced-motion: reduce) {
  .sheet-enter-active,
  .sheet-leave-active {
    transition: none;
  }
}
</style>
