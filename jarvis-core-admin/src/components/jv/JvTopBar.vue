<script setup lang="ts">
// Core Admin top bar: brand on the left, the current node title in the
// centre, and the session cluster (right slot) supplied by the shell.
defineProps<{ title: string; subtitle?: string }>();
</script>

<template>
  <header class="jv-topbar">
    <div class="left">
      <div class="brand">
        <span class="name">Jarvis</span>
        <span class="tag">Core Administration</span>
      </div>
    </div>

    <div class="center">
      <div class="title">
        <h1>{{ title }}</h1>
        <p v-if="subtitle">{{ subtitle }}</p>
      </div>
    </div>

    <div class="right"><slot /></div>
  </header>
</template>

<style scoped>
.jv-topbar {
  position: relative;
  z-index: 5;
  display: grid;
  grid-template-columns: auto minmax(0, 1fr) auto;
  align-items: center;
  gap: 24px;
  padding: 20px 32px 0 32px;
}
.left, .right { display: flex; align-items: center; }
.center { min-width: 0; }
.right { justify-content: flex-end; gap: 16px; }

.brand { display: flex; flex-direction: column; gap: 2px; min-width: 0; }
.name { font-size: 17px; line-height: 1.3; font-weight: 600; color: var(--text-primary); }
.tag { font-size: 12px; line-height: 1.4; color: var(--text-secondary); }

.title { min-width: 0; text-align: center; }
.title h1 { margin: 0; font-size: 18px; line-height: 1.3; font-weight: 600; letter-spacing: -0.01em; color: var(--text-primary); }
.title p { margin: 2px 0 0; font-size: 13px; line-height: 1.45; color: var(--text-secondary); }

@media (max-width: 1099px), (max-height: 759px) {
  .jv-topbar { padding: 14px 16px 0; gap: 12px; }
  .tag { display: none; }
  .title p { display: none; }
}
</style>
