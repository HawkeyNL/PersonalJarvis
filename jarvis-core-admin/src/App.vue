<script setup lang="ts">
import { computed, onBeforeUnmount, onMounted, ref } from "vue";
import { api, errorText, type ViewName } from "./admin";
import NavIcon from "./components/NavIcon.vue";
import AgentsView from "./views/AgentsView.vue";
import CredentialsView from "./views/CredentialsView.vue";
import HealthView from "./views/HealthView.vue";
import LogsView from "./views/LogsView.vue";
import ModelsView from "./views/ModelsView.vue";
import UsageView from "./views/UsageView.vue";
import OverviewView from "./views/OverviewView.vue";
import ServicesView from "./views/ServicesView.vue";
import SystemView from "./views/SystemView.vue";
import UpdateView from "./views/UpdateView.vue";

const navSections: { label: string; items: { id: ViewName; label: string }[] }[] = [
  { label: "Home", items: [{ id: "overview", label: "Overview" }] },
  { label: "Operations", items: [
    { id: "health", label: "Health" }, { id: "services", label: "Services" }, { id: "logs", label: "Logs" },
  ] },
  { label: "Intelligence", items: [
    { id: "agents", label: "Agents" }, { id: "models", label: "Models" }, { id: "usage", label: "Usage & Costs" },
  ] },
  { label: "Administration", items: [
    { id: "credentials", label: "Credentials" }, { id: "update", label: "Update" }, { id: "system", label: "System" },
  ] },
];
const items = navSections.flatMap((section) => section.items);
const views = { overview: OverviewView, health: HealthView, services: ServicesView, update: UpdateView, agents: AgentsView, models: ModelsView, usage: UsageView, credentials: CredentialsView, logs: LogsView, system: SystemView };
const IDLE_TIMEOUT_MS = 5 * 60 * 1000;
const TOUCH_INTERVAL_MS = 5 * 1000;
const active = ref<ViewName>("overview");
const clock = ref("");
const locked = ref(true);
const authBusy = ref(false);
const authError = ref("");
const idleSeconds = ref(300);
const restartRequired = ref(false);
const restartBusy = ref(false);
const restartError = ref("");
const current = computed(() => views[active.value]);
let clockTimer: number | undefined;
let idleTimer: number | undefined;
let runtimeTimer: number | undefined;
let lastActivity = Date.now();
let lastBrokerTouch = 0;
let touchPending = false;
function tick() { clock.value = new Date().toLocaleTimeString([], { hour12: false }); }
async function unlock() {
  authBusy.value = true;
  authError.value = "";
  try {
    const status = await api.sessionAuthenticate();
    if (!status.authenticated) throw new Error("Administrator authentication did not complete.");
    locked.value = false;
    lastActivity = Date.now();
    lastBrokerTouch = lastActivity;
    idleSeconds.value = 300;
    await checkRuntime();
  } catch (error) {
    locked.value = true;
    authError.value = errorText(error);
  } finally {
    authBusy.value = false;
  }
}
async function checkRuntime() {
  if (locked.value || restartRequired.value) return;
  try {
    const status = await api.runtimeStatus();
    if (status.restart_required) restartRequired.value = true;
  } catch {
    // Update operations still raise the mandatory restart state directly.
  }
}
async function restartNow() {
  if (restartBusy.value) return;
  restartBusy.value = true;
  restartError.value = "";
  try {
    await api.restartApp();
  } catch (error) {
    restartError.value = errorText(error);
    restartBusy.value = false;
  }
}
function lock(reason = "Locked after five minutes without activity.") {
  if (locked.value) return;
  locked.value = true;
  authError.value = reason;
  idleSeconds.value = 0;
  void api.sessionLock().catch(() => undefined);
}
function recordActivity() {
  if (locked.value) return;
  const now = Date.now();
  if (now - lastActivity >= IDLE_TIMEOUT_MS) {
    lock();
    return;
  }
  lastActivity = now;
  idleSeconds.value = 300;
  if (touchPending || now - lastBrokerTouch < TOUCH_INTERVAL_MS) return;
  touchPending = true;
  lastBrokerTouch = now;
  void api.sessionTouch()
    .catch(() => lock("The administrator session ended. Authenticate again."))
    .finally(() => { touchPending = false; });
}
function checkIdle() {
  if (locked.value) return;
  const remaining = IDLE_TIMEOUT_MS - (Date.now() - lastActivity);
  idleSeconds.value = Math.max(0, Math.ceil(remaining / 1000));
  if (remaining <= 0) lock();
}
onMounted(() => {
  tick();
  clockTimer = window.setInterval(tick, 1000);
  idleTimer = window.setInterval(checkIdle, 1000);
  runtimeTimer = window.setInterval(() => void checkRuntime(), 15_000);
  void unlock();
});
onBeforeUnmount(() => {
  clearInterval(clockTimer);
  clearInterval(idleTimer);
  clearInterval(runtimeTimer);
  if (!locked.value) void api.sessionLock().catch(() => undefined);
});
</script>
<template>
  <div class="app-shell" @pointermove="recordActivity" @pointerdown="recordActivity" @mouseenter="recordActivity" @wheel="recordActivity" @touchstart="recordActivity" @keydown="recordActivity" @focusin="recordActivity">
    <aside class="sidebar">
      <div class="brand"><div class="brand-mark"><i /></div><div><strong>JARVIS</strong><span>CORE ADMIN</span></div></div>
      <nav aria-label="Administration sections"><div v-for="section in navSections" :key="section.label" class="nav-section"><span class="nav-category">{{ section.label }}</span><button v-for="item in section.items" :key="item.id" :disabled="locked" :class="{ active: active === item.id }" @click="active = item.id"><NavIcon :name="item.id" /><span>{{ item.label }}</span></button></div></nav>
      <div class="security-boundary"><span :class="['status-light', { locked }]" />{{ locked ? "Administration locked" : "Authenticated session" }}<small>{{ locked ? "Unlock through system authorization" : `Locks after inactivity · ${idleSeconds}s` }}</small></div>
    </aside>
    <div class="main-shell">
      <header class="topbar"><div><span class="topbar-kicker">JARVIS HOME NODE</span><strong>{{ locked ? "Locked" : items.find((item) => item.id === active)?.label }}</strong></div><div class="topbar-actions"><button v-if="!locked" class="small secondary" @click="lock('Locked by owner.')">Lock</button><time>{{ clock }}</time></div></header>
      <main :class="['page', { 'logs-active': active === 'logs', 'locked-page': locked }]">
        <component :is="current" v-if="!locked" @restart-required="restartRequired = true" />
        <section v-else class="lock-screen" aria-live="polite">
          <div class="lock-mark"><i /></div>
          <span class="topbar-kicker">PRIVILEGED ADMINISTRATION</span>
          <h1>Jarvis Core is locked</h1>
          <p>Authenticate once through the GNOME system dialog. Your password is never handled by this application.</p>
          <div v-if="authError" class="lock-error">{{ authError }}</div>
          <button :disabled="authBusy" @click="unlock">{{ authBusy ? "Waiting for system authorization…" : "Unlock administration" }}</button>
          <small>The session locks after five minutes without pointer or keyboard activity.</small>
        </section>
      </main>
    </div>
    <div v-if="restartRequired" class="dialog-backdrop restart-backdrop">
      <section class="dialog restart-dialog" role="alertdialog" aria-modal="true" aria-labelledby="restart-title">
        <span class="eyebrow">UPDATE INSTALLED</span>
        <h2 id="restart-title">Restart Jarvis Core Administration</h2>
        <p>The trusted update completed and replaced administration components. This older application process cannot continue safely.</p>
        <p v-if="restartError" class="restart-error">{{ restartError }}</p>
        <div class="dialog-actions"><button :disabled="restartBusy" @click="restartNow">{{ restartBusy ? "Restarting…" : "Restart now" }}</button></div>
      </section>
    </div>
  </div>
</template>
