<script setup lang="ts">
import { ref } from "vue";
import { invoke } from "@tauri-apps/api/core";

const name = ref("");
const greetMsg = ref("");

async function greet() {
  // Proves the JS -> Rust bridge works. See src-tauri/src/lib.rs.
  greetMsg.value = await invoke<string>("greet", { name: name.value });
}
</script>

<template>
  <section class="view">
    <h1>Welkom bij Jarvis</h1>
    <p class="muted">
      Fase 0 — client-skelet. Tauri 2 + Vue 3 + TypeScript, met Pinia en Vue
      Router. De Rust&#8596;JS-brugdemo hieronder bevestigt dat de native laag werkt.
    </p>

    <form class="greet" @submit.prevent="greet">
      <input v-model="name" placeholder="Je naam…" aria-label="naam" />
      <button type="submit">Groet via Rust</button>
    </form>
    <p v-if="greetMsg" class="greet-msg">{{ greetMsg }}</p>
  </section>
</template>
