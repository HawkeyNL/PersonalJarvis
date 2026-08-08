<script setup lang="ts">
import { ref, onMounted } from "vue";
import { getJson } from "../api";
import {
  currentSession,
  login,
  logout,
  listDevices,
  type DeviceItem,
} from "../auth";

const backend = ref<"checking" | "ok" | "fout">("checking");
const auth = ref<"checking" | "in" | "uit" | "fout">("checking");
const devices = ref<DeviceItem[]>([]);
const error = ref<string | null>(null);

async function refresh() {
  error.value = null;
  try {
    await getJson("/readyz");
    backend.value = "ok";
  } catch {
    backend.value = "fout";
  }

  try {
    let session = await currentSession();
    // Dev convenience: auto enroll + log in on first launch.
    if (!session.token) {
      await login();
      session = await currentSession();
    }
    if (session.token) {
      devices.value = await listDevices(session.token);
      auth.value = "in";
    } else {
      auth.value = "uit";
    }
  } catch (e) {
    auth.value = "fout";
    error.value = String(e);
  }
}

async function doLogout() {
  await logout();
  devices.value = [];
  auth.value = "uit";
}

onMounted(refresh);
</script>

<template>
  <section class="view">
    <h1>Welkom bij Jarvis</h1>
    <p class="muted">
      Client-skelet met live backend-koppeling en device-bound login. De
      privésleutel wordt in Rust bewaard (nooit in de webview) en tekent de
      login-challenge.
    </p>

    <div class="badge">
      <span
        class="dot"
        :class="backend === 'ok' ? 'dot-ok' : backend === 'fout' ? 'dot-err' : 'dot-todo'"
      ></span>
      Backend:
      {{ backend === "ok" ? "verbonden" : backend === "fout" ? "niet bereikbaar" : "controleren…" }}
    </div>
    <div class="badge">
      <span
        class="dot"
        :class="auth === 'in' ? 'dot-ok' : auth === 'fout' ? 'dot-err' : 'dot-todo'"
      ></span>
      Login:
      {{ auth === "in" ? "ingelogd (device-bound)" : auth === "fout" ? "mislukt" : auth === "uit" ? "uitgelogd" : "bezig…" }}
    </div>

    <div v-if="auth === 'in'" class="devices">
      <h2>Vertrouwde apparaten</h2>
      <ul class="status-list">
        <li v-for="d in devices" :key="d.id">
          <span class="dot dot-ok"></span>
          {{ d.name }} — <code>{{ d.platform }}</code> ({{ d.status }})
        </li>
      </ul>
      <button @click="doLogout">Uitloggen</button>
    </div>

    <div v-else-if="auth === 'uit'">
      <button @click="refresh">Inloggen</button>
    </div>

    <p v-if="error" class="muted err">Laatste fout: {{ error }}</p>
  </section>
</template>
