<script setup lang="ts">
import NavIcon from "./components/NavIcon.vue";

const tabs = [
  { to: "/", label: "Jarvis", icon: "core" as const },
  { to: "/portfolio", label: "Portfolio", icon: "chart" as const },
  { to: "/broker", label: "IBKR", icon: "link" as const },
  { to: "/status", label: "System", icon: "pulse" as const },
  { to: "/settings", label: "Settings", icon: "gear" as const },
];
</script>

<template>
  <div class="app">
    <main class="content">
      <RouterView />
    </main>

    <!-- Floating liquid-glass dock (all screens). Jarvis is the primary tab. -->
    <nav class="dock">
      <RouterLink
        v-for="t in tabs"
        :key="t.to"
        :to="t.to"
        class="tab"
        :class="{ primary: t.to === '/' }"
      >
        <NavIcon :name="t.icon" />
        <span>{{ t.label }}</span>
      </RouterLink>
    </nav>
  </div>
</template>
