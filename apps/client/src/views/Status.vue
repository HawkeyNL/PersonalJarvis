<script setup lang="ts">
import { ref, onMounted } from "vue";
import { API_BASE, getJson } from "../api";

type Health = { status: string; environment?: string };
type Check = "checking" | "ok" | "fout";

const livez = ref<Check>("checking");
const readyz = ref<Check>("checking");
const environment = ref<string | null>(null);
const error = ref<string | null>(null);

function dotClass(c: Check): string {
  if (c === "ok") return "dot-ok";
  if (c === "fout") return "dot-err";
  return "dot-todo";
}

async function check() {
  livez.value = "checking";
  readyz.value = "checking";
  environment.value = null;
  error.value = null;

  try {
    await getJson<Health>("/livez");
    livez.value = "ok";
  } catch (e) {
    livez.value = "fout";
    error.value = String(e);
  }

  try {
    const r = await getJson<Health>("/readyz");
    readyz.value = "ok";
    environment.value = r.environment ?? null;
  } catch (e) {
    readyz.value = "fout";
    error.value = String(e);
  }
}

onMounted(check);
</script>

<template>
  <section class="view">
    <h1>Status</h1>
    <p class="muted">
      Live-verbinding met de backend <code>jarvis-api</code> op
      <code>{{ API_BASE }}</code> (via de Tauri HTTP-plugin).
    </p>

    <ul class="status-list">
      <li>
        <span class="dot" :class="dotClass(livez)"></span>
        Liveness <code>/livez</code> — {{ livez }}
      </li>
      <li>
        <span class="dot" :class="dotClass(readyz)"></span>
        Readiness <code>/readyz</code> — {{ readyz }}
        <span v-if="environment">· {{ environment }}</span>
      </li>
    </ul>

    <button @click="check">Opnieuw controleren</button>
    <p v-if="error" class="muted err">Laatste fout: {{ error }}</p>
  </section>
</template>
