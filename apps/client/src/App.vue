<script setup lang="ts">
import { ref, computed, onMounted, onBeforeUnmount } from "vue";
import { useRoute } from "vue-router";
import NavIcon from "./components/NavIcon.vue";

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
});
onBeforeUnmount(() => clearInterval(timer));
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
  </div>
</template>
