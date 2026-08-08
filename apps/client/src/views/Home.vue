<script setup lang="ts">
import { ref, onMounted } from "vue";
import { invoke } from "@tauri-apps/api/core";
import { getJson } from "../api";

const name = ref("");
const greetMsg = ref("");
const backend = ref<"checking" | "ok" | "fout">("checking");

async function greet() {
  // Proves the JS -> Rust bridge works. See src-tauri/src/lib.rs.
  greetMsg.value = await invoke<string>("greet", { name: name.value });
}

async function checkBackend() {
  try {
    await getJson("/readyz");
    backend.value = "ok";
  } catch {
    backend.value = "fout";
  }
}

onMounted(checkBackend);
</script>

<template>
  <section class="view">
    <h1>Welkom bij Jarvis</h1>
    <p class="muted">
      Client-skelet met live backend-koppeling. Tauri 2 + Vue 3 + TypeScript
      (Pinia, Vue Router). Rust&#8596;JS-brug en de HTTP-plugin werken op macOS én iOS.
    </p>

    <div class="badge">
      <span
        class="dot"
        :class="backend === 'ok' ? 'dot-ok' : backend === 'fout' ? 'dot-err' : 'dot-todo'"
      ></span>
      Backend:
      {{
        backend === "ok"
          ? "verbonden"
          : backend === "fout"
            ? "niet bereikbaar"
            : "controleren…"
      }}
    </div>

    <form class="greet" @submit.prevent="greet">
      <input v-model="name" placeholder="Je naam…" aria-label="naam" />
      <button type="submit">Groet via Rust</button>
    </form>
    <p v-if="greetMsg" class="greet-msg">{{ greetMsg }}</p>
  </section>
</template>
