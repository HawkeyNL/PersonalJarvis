<script setup lang="ts">
import { ref, computed, onMounted, onBeforeUnmount } from "vue";
import { useRoute } from "vue-router";
import NavIcon from "./components/NavIcon.vue";
import AppLock from "./components/AppLock.vue";
import UnlockApprovals from "./components/UnlockApprovals.vue";
import PairingApprovals from "./components/PairingApprovals.vue";
import { locked, initLock, noteActivity } from "./lock";
import { startApprovalPolling, stopApprovalPolling } from "./unlockApprovals";
import { startPairingPolling, stopPairingPolling } from "./pairingApprovals";
import { maybeStartWake, stopWake } from "./voicewake";

const route = useRoute();
const mode = computed<"system" | "trading">(() =>
  route.path.startsWith("/trading") ? "trading" : "system",
);

// Primary modes — top bar.
const modes = [
  { key: "system", label: "SYSTEM", to: "/" },
  { key: "trading", label: "TRADING", to: "/trading" },
];

// Contextual sub-tabs — bottom dock, per mode.
const systemTabs = [
  { to: "/", label: "Jarvis", icon: "core" as const },
  { to: "/status", label: "System", icon: "pulse" as const },
  { to: "/settings", label: "Settings", icon: "gear" as const },
];
const tradingTabs = [
  { to: "/trading", label: "Portfolio", icon: "chart" as const },
  { to: "/trading/ibkr", label: "IBKR", icon: "link" as const },
];
const subTabs = computed(() => (mode.value === "trading" ? tradingTabs : systemTabs));

// Global clock in the top bar.
const clock = ref("--:--:--");
let timer: number | undefined;
function tick() {
  const d = new Date();
  const p = (n: number) => String(n).padStart(2, "0");
  clock.value = `${p(d.getHours())}:${p(d.getMinutes())}:${p(d.getSeconds())}`;
}
onMounted(() => {
  tick();
  timer = window.setInterval(tick, 1000);
  initLock();
  startApprovalPolling(); // this device can approve other devices' unlocks
  startPairingPolling();
  maybeStartWake(); // resume "Hey Jarvis" if it was enabled
  window.addEventListener("pointerdown", noteActivity, { passive: true });
  window.addEventListener("keydown", noteActivity);
});
onBeforeUnmount(() => {
  clearInterval(timer);
  stopApprovalPolling();
  stopPairingPolling();
  stopWake();
  window.removeEventListener("pointerdown", noteActivity);
  window.removeEventListener("keydown", noteActivity);
});
</script>

<template>
  <div class="app">
    <header class="topbar">
      <nav class="modeswitch">
        <RouterLink
          v-for="m in modes"
          :key="m.key"
          :to="m.to"
          class="mode"
          :class="{ on: mode === m.key }"
        >
          {{ m.label }}
        </RouterLink>
      </nav>
      <span class="topclock">{{ clock }}</span>
    </header>

    <main class="content">
      <RouterView />
    </main>

    <!-- Contextual sub-tabs (liquid glass), bottom-centre. -->
    <nav class="subdock">
      <RouterLink v-for="t in subTabs" :key="t.to" :to="t.to" class="subtab">
        <NavIcon :name="t.icon" />
        <span>{{ t.label }}</span>
      </RouterLink>
    </nav>

    <!-- Incoming unlock approvals for other devices (phone side). -->
    <UnlockApprovals />
    <PairingApprovals />
    <!-- Full-screen biometric gate on desktop when the lock is enabled. -->
    <AppLock v-if="locked" />
  </div>
</template>
