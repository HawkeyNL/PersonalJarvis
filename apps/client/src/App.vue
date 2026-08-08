<script setup lang="ts">
import { useAppStore } from "./stores/app";

const app = useAppStore();
</script>

<template>
  <div class="app">
    <aside class="sidebar">
      <div class="brand">
        <svg class="brand-mark" viewBox="0 0 24 24" width="18" height="18" aria-hidden="true">
          <path d="M12 2 22 12 12 22 2 12Z" fill="currentColor" />
        </svg>
        <span class="brand-name">{{ app.name }}</span>
      </div>
      <nav class="nav">
        <RouterLink to="/" class="nav-link">Home</RouterLink>
        <RouterLink to="/portfolio" class="nav-link">Portfolio</RouterLink>
        <RouterLink to="/status" class="nav-link">Status</RouterLink>
      </nav>
      <p class="tagline">{{ app.tagline }}</p>
    </aside>
    <main class="content">
      <RouterView />
    </main>
  </div>
</template>
