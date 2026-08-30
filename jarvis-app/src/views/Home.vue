<script setup lang="ts">
import { ref, computed, onMounted, onBeforeUnmount } from "vue";
import { API_BASE, getJson, ApiError } from "../api";
import { currentSession, login, clearSession, listDevices, PairingPending } from "../auth";
import ReactorCore from "../components/ReactorCore.vue";
import JarvisConsole from "../components/JarvisConsole.vue";

// The homepage is pure Jarvis: a living backdrop + a hover-reveal console.
// Backend health and device-bound login run silently in the background so the
// chat always has a valid session; there is no telemetry UI here anymore
// (System/Trading tabs own that).
const backend = ref<"checking" | "ok" | "fout">("checking");
const auth = ref<"checking" | "in" | "uit" | "wachten" | "fout">("checking");
const online = computed(() => backend.value === "ok");

let pollTimer: number | undefined;
let authTrying = false;

async function pollBackend() {
  try {
    await getJson("/readyz");
    backend.value = "ok";
    if (auth.value !== "in") await refreshAuth(); // retry login once the backend is up
  } catch {
    backend.value = "fout";
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

onMounted(() => {
  pollBackend();
  pollTimer = window.setInterval(pollBackend, 5000);
});
onBeforeUnmount(() => clearInterval(pollTimer));
</script>

<template>
  <section class="jarvis-home">
    <!-- Full-bleed living backdrop. -->
    <div class="backdrop">
      <ReactorCore name="Jarvis" :active="online" />
    </div>
    <p v-if="auth === 'wachten'" class="pairing-wait">
      Wacht op goedkeuring vanaf een vertrouwd Jarvis-apparaat.
    </p>
    <p v-else-if="backend === 'fout'" class="connection-error" role="status">
      Home Node niet bereikbaar op {{ API_BASE }}. Controleer het netwerk en probeer opnieuw.
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
