<script setup lang="ts">
import { ref, computed, onMounted, onBeforeUnmount } from "vue";
import { getJson, ApiError } from "../api";
import { currentSession, login, clearSession, listDevices, PairingPending } from "../auth";
import { configureHomeNode, homeNodeConfig, loadHomeNodeConfig } from "../homeNode";
import ReactorCore from "../components/ReactorCore.vue";
import JarvisConsole from "../components/JarvisConsole.vue";

// The homepage is pure Jarvis: a living backdrop + a hover-reveal console.
// Backend health and device-bound login run silently in the background so the
// chat always has a valid session; there is no telemetry UI here anymore
// (System/Trading tabs own that).
const backend = ref<"unconfigured" | "checking" | "ok" | "fout">("checking");
const auth = ref<"checking" | "in" | "uit" | "wachten" | "fout">("checking");
const online = computed(() => backend.value === "ok");

let pollTimer: number | undefined;
let authTrying = false;
const originInput = ref("");
const configBusy = ref(false);
const configError = ref<string | null>(null);

async function pollBackend() {
  if (!homeNodeConfig.value.configured) {
    backend.value = "unconfigured";
    return;
  }
  try {
    await getJson("/readyz");
    backend.value = "ok";
    if (auth.value !== "in") await refreshAuth(); // retry login once the backend is up
  } catch {
    backend.value = "fout";
  }
}

function startBackendPolling() {
  if (pollTimer !== undefined) return;
  pollTimer = window.setInterval(pollBackend, 5000);
}

async function saveHomeNode() {
  configBusy.value = true;
  configError.value = null;
  try {
    await configureHomeNode(originInput.value);
    backend.value = "checking";
    await pollBackend();
    startBackendPolling();
  } catch (error) {
    configError.value = error instanceof Error ? error.message : String(error);
  } finally {
    configBusy.value = false;
  }
}

// Return a usable session token, logging in (enroll if needed) when absent.
async function ensureSession(): Promise<string | null> {
  let session = await currentSession();
  if (!session.token) {
    await login();
    session = await currentSession();
  }
  return session.token;
}

async function refreshAuth() {
  if (authTrying) return;
  authTrying = true;
  try {
    let token = await ensureSession();
    if (!token) {
      auth.value = "uit";
      return;
    }
    // Listing devices validates the token; a stale one (backend restarted) 401s
    // — drop it and log in fresh once, instead of looping on a dead token.
    try {
      await listDevices(token);
    } catch (e) {
      if (e instanceof ApiError && e.status === 401) {
        await clearSession();
        token = await ensureSession();
        if (!token) {
          auth.value = "uit";
          return;
        }
        await listDevices(token);
      } else {
        throw e;
      }
    }
    auth.value = "in";
  } catch (error) {
    auth.value = error instanceof PairingPending ? "wachten" : "fout";
  } finally {
    authTrying = false;
  }
}

onMounted(async () => {
  const config = await loadHomeNodeConfig();
  originInput.value = config.origin ?? "";
  if (config.configured) {
    await pollBackend();
    startBackendPolling();
  } else {
    backend.value = "unconfigured";
  }
});
onBeforeUnmount(() => clearInterval(pollTimer));
</script>

<template>
  <section class="jarvis-home">
    <!-- Full-bleed living backdrop. -->
    <div class="backdrop">
      <ReactorCore name="Jarvis" :active="online" />
    </div>
    <form v-if="backend === 'unconfigured'" class="connection-setup glass" @submit.prevent="saveHomeNode">
      <h2>Verbind met je Home Node</h2>
      <p>Voer de HTTPS-origin van je Home Node in. Jarvis bewaart alleen dit adres lokaal; geen credentials.</p>
      <label for="home-node-origin">Home Node-origin</label>
      <input
        id="home-node-origin"
        v-model.trim="originInput"
        type="url"
        inputmode="url"
        autocomplete="url"
        placeholder="https://jarvis.home.example"
        required
      />
      <button type="submit" :disabled="configBusy">
        {{ configBusy ? "Verbinden…" : "Verbinden en koppelen" }}
      </button>
      <p v-if="configError" class="config-error" role="alert">{{ configError }}</p>
      <p class="setup-hint">Lokale HTTP op localhost is uitsluitend toegestaan in een development-build.</p>
    </form>
    <p v-if="auth === 'wachten'" class="pairing-wait">
      Wacht op goedkeuring vanaf een vertrouwd Jarvis-apparaat.
    </p>
    <p v-else-if="backend === 'fout'" class="connection-error" role="status">
      Home Node niet bereikbaar op {{ homeNodeConfig.origin }}. Controleer het netwerk en probeer opnieuw.
    </p>
    <!-- Floating conversation + hover-reveal input. -->
    <JarvisConsole />
  </section>
</template>

<style scoped>
.jarvis-home {
  position: relative;
  height: 100%;
  min-height: 100%;
}
.pairing-wait {
  position: relative;
  z-index: 1;
  margin: 2rem auto;
  max-width: 28rem;
  text-align: center;
}
.connection-error {
  position: relative;
  z-index: 3;
  width: min(34rem, calc(100% - 2rem));
  margin: 1rem auto;
  padding: 0.75rem 1rem;
  border: 1px solid rgba(248, 113, 113, 0.45);
  border-radius: 0.75rem;
  background: rgba(30, 8, 8, 0.78);
  color: #fecaca;
  text-align: center;
}
.connection-setup {
  position: relative;
  z-index: 4;
  width: min(32rem, calc(100% - 2rem));
  margin: 3rem auto;
  padding: 1.25rem;
}
.connection-setup h2 { margin-top: 0; }
.connection-setup label { display: block; margin: 1rem 0 0.4rem; }
.connection-setup input { width: 100%; box-sizing: border-box; margin-bottom: 0.8rem; }
.config-error { color: #fecaca; }
.setup-hint { color: var(--muted); font-size: 0.82rem; }

/* The core sits fixed behind everything (under the translucent top bar/dock),
   so the whole screen reads as one living surface. */
.backdrop {
  position: fixed;
  inset: 0;
  z-index: 0;
  display: grid;
  place-items: center;
  --core-size: min(80vh, 780px);
  pointer-events: none;
}
.backdrop::before {
  content: "";
  position: absolute;
  inset: 0;
  background:
    radial-gradient(ellipse at 50% 44%, rgba(52, 245, 160, 0.1), transparent 60%),
    linear-gradient(rgba(52, 245, 160, 0.028) 1px, transparent 1px) 0 0 / 46px 46px,
    linear-gradient(90deg, rgba(52, 245, 160, 0.028) 1px, transparent 1px) 0 0 / 46px 46px;
  mask-image: radial-gradient(ellipse at 55% 45%, #000 25%, transparent 82%);
  -webkit-mask-image: radial-gradient(ellipse at 55% 45%, #000 25%, transparent 82%);
}
</style>
