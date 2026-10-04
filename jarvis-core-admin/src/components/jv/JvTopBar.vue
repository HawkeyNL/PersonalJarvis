<script setup lang="ts">
// Core Admin top bar: brand on the left, the current node title in the
// centre, and the session cluster (right slot) supplied by the shell.
defineProps<{ title: string; subtitle?: string }>();
</script>

<template>
  <header class="jv-topbar">
    <div class="left">
      <div class="brand">
        <span class="ring" aria-hidden="true"></span>
        <span class="words">
          <span class="name">JARVIS</span>
          <span class="tag">CORE ADMINISTRATION</span>
        </span>
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
  grid-template-columns: minmax(0, 1fr) auto minmax(0, 1fr);
  align-items: center;
  gap: 24px;
  padding: 26px 32px 0 36px;
}
.left, .right { display: flex; align-items: center; min-width: 0; }
.right { justify-content: flex-end; gap: 22px; }

.brand { display: flex; align-items: center; gap: 22px; text-decoration: none; color: inherit; }
.ring {
  width: 34px; height: 34px; flex: none; box-sizing: border-box; border-radius: 50%;
  border: 3px solid var(--accent);
  box-shadow: 0 0 14px rgba(var(--accent-rgb), 0.7), inset 0 0 8px rgba(var(--accent-rgb), 0.45);
}
.words { display: flex; flex-direction: column; }
.name { font-family: var(--font-display); font-weight: 500; font-size: 19px; letter-spacing: 0.5em; line-height: 24px; color: #eaf7f1; }
.tag { margin-top: 2px; font-size: 11px; letter-spacing: 0.32em; color: var(--text-5); }

.title { text-align: center; position: relative; }
.title h1 {
  margin: 0; font-family: var(--font-display); font-weight: 600; font-size: var(--fs-23);
  letter-spacing: 0.5em; padding-left: 0.5em; line-height: 30px; color: var(--text-0);
}
.title p { margin: 10px 0 0; font-size: 11px; letter-spacing: 0.38em; padding-left: 0.38em; color: #9fb8ad; }
.title::before, .title::after {
  content: ""; position: absolute; top: 20px; width: 68px; height: 1px;
}
.title::before { right: calc(100% + 0px); background: linear-gradient(90deg, transparent, rgba(var(--accent-rgb), 0.6)); }
.title::after { left: calc(100% + 0px); background: linear-gradient(90deg, rgba(var(--accent-rgb), 0.6), transparent); }

/* The session cluster is wider than the desktop app's; drop the title rules
   before they run into it. */
@media (max-width: 1399px) {
  .title::before, .title::after { display: none; }
}
@media (max-width: 1099px), (max-height: 759px) {
  .jv-topbar { padding: 16px 16px 0; gap: 12px; grid-template-columns: auto minmax(0, 1fr) auto; }
  .tag { display: none; }
  .ring { width: 28px; height: 28px; }
  .name { font-size: 18px; letter-spacing: 0.42em; }
  .title h1 { font-size: 19px; letter-spacing: 0.32em; }
  .title p { display: none; }
}
</style>
