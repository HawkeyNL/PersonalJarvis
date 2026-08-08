<script setup lang="ts">
import { useAppStore } from "./stores/app";

const app = useAppStore();
</script>

<template>
  <div class="app">
    <aside class="sidebar">
      <div class="brand">
        <span class="brand-mark">◆</span>
        <span class="brand-name">{{ app.name }}</span>
      </div>
      <nav class="nav">
        <RouterLink to="/" class="nav-link">Home</RouterLink>
        <RouterLink to="/status" class="nav-link">Status</RouterLink>
      </nav>
      <p class="tagline">{{ app.tagline }}</p>
    </aside>
    <main class="content">
      <RouterView />
    </main>
  </div>
</template>
