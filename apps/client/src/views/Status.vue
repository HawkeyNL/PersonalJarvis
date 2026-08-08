<script setup lang="ts">
// Placeholder. The backend (jarvis-api) runs separately on localhost:8080.
// Wiring the client to /livez + /readyz (via the Tauri HTTP plugin) is next.
const apiUrl = "http://localhost:8080";
</script>

<template>
  <section class="view">
    <h1>Status</h1>
    <p class="muted">
      De backend <code>jarvis-api</code> draait apart op <code>{{ apiUrl }}</code>.
      Koppeling aan <code>/livez</code> en <code>/readyz</code> volgt in de
      volgende stap.
    </p>
    <ul class="status-list">
      <li><span class="dot dot-todo"></span> API-koppeling — nog te doen</li>
      <li><span class="dot dot-todo"></span> Auth &amp; device-registratie — Fase 1</li>
      <li><span class="dot dot-todo"></span> Lokale versleutelde cache — Fase 1</li>
    </ul>
  </section>
</template>
