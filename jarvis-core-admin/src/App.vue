<script setup lang="ts">
import { computed, onBeforeUnmount, onMounted, ref } from "vue";
import { api, errorText, type ViewName } from "./admin";
import NavIcon, { type IconName } from "./components/NavIcon.vue";
import JvBackdrop from "./components/jv/JvBackdrop.vue";
import JvOrb from "./components/jv/JvOrb.vue";
import JvPanel from "./components/jv/JvPanel.vue";
import JvSegmented from "./components/jv/JvSegmented.vue";
import JvStatusDot from "./components/jv/JvStatusDot.vue";
import JvTopBar from "./components/jv/JvTopBar.vue";
import AgentsView from "./views/AgentsView.vue";
import CredentialsView from "./views/CredentialsView.vue";
import AiAccountsView from "./views/AiAccountsView.vue";
import DevicesView from "./views/DevicesView.vue";
import HealthView from "./views/HealthView.vue";
import LogsView from "./views/LogsView.vue";
import ModelsView from "./views/ModelsView.vue";
import UsageView from "./views/UsageView.vue";
import OverviewView from "./views/OverviewView.vue";
import ServicesView from "./views/ServicesView.vue";
import SystemView from "./views/SystemView.vue";
import UpdateView from "./views/UpdateView.vue";

// Four nodes, as in the redesign; each node's views are tabs. A slim rail
// replaces the hub/satellite stage of the desktop NodePage: that stage takes
// ~420px of height, which leaves no room for the log viewport and the model
// tables in this 820px (minimum 620px) window.
type NodeId = "overview" | "operations" | "intelligence" | "administration";
const nodes: { id: NodeId; label: string; subtitle: string; icon: IconName; views: { id: ViewName; label: string; icon: IconName }[] }[] = [
  { id: "overview", label: "Overview", subtitle: "HOME NODE AT A GLANCE", icon: "core", views: [
    { id: "overview", label: "Overview", icon: "core" },
  ] },
  { id: "operations", label: "Operations", subtitle: "HEALTH · SERVICES · LOGS", icon: "pulse", views: [
    { id: "health", label: "Health", icon: "pulse" }, { id: "services", label: "Services", icon: "layers" }, { id: "logs", label: "Logs", icon: "lines" },
  ] },
  { id: "intelligence", label: "Intelligence", subtitle: "AGENTS · MODELS · USAGE", icon: "bulb", views: [
    { id: "agents", label: "Agents", icon: "agents" }, { id: "models", label: "Models", icon: "chip" }, { id: "usage", label: "Usage & Costs", icon: "chart" },
  ] },
  { id: "administration", label: "Administration", subtitle: "DEVICES · ACCOUNTS · UPDATES", icon: "shield", views: [
    { id: "devices", label: "Devices", icon: "monitor" }, { id: "ai-accounts", label: "AI Accounts", icon: "link-2" }, { id: "credentials", label: "Credentials", icon: "key" }, { id: "update", label: "Update", icon: "download" }, { id: "system", label: "System", icon: "gear" },
  ] },
];
const views = { overview: OverviewView, health: HealthView, services: ServicesView, update: UpdateView, agents: AgentsView, models: ModelsView, usage: UsageView, credentials: CredentialsView, "ai-accounts": AiAccountsView, devices: DevicesView, logs: LogsView, system: SystemView };
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
const node = computed(() => nodes.find((item) => item.views.some((view) => view.id === active.value)) ?? nodes[0]);
const activeView = computed({ get: () => active.value, set: (id: string) => { active.value = id as ViewName; } });
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
    <JvBackdrop glow-y="34%" horizon="28px" />
    <nav class="rail" aria-label="Administration nodes">
      <button v-for="item in nodes" :key="item.id" type="button" :disabled="locked" :class="{ on: !locked && node.id === item.id }" :aria-current="!locked && node.id === item.id ? 'page' : undefined" @click="active = item.views[0].id">
        <span class="rail-icon" aria-hidden="true"><NavIcon :name="item.icon" /></span>
        <span>{{ item.label }}</span>
      </button>
    </nav>
    <div class="main-shell">
      <JvTopBar :title="locked ? 'LOCKED' : node.label.toUpperCase()" :subtitle="locked ? 'PRIVILEGED ADMINISTRATION' : node.subtitle">
        <div class="session-cluster">
          <JvStatusDot :tone="locked ? 'warn' : 'ok'" :label="locked ? 'Locked' : 'Authenticated'" />
          <span v-if="!locked" class="session-idle">locks in {{ idleSeconds }}s</span>
          <span class="sep" aria-hidden="true"></span>
          <span class="session-clock"><NavIcon name="clock" /><time>{{ clock }}</time></span>
          <button v-if="!locked" class="small" @click="lock('Locked by owner.')">Lock</button>
        </div>
      </JvTopBar>
      <div v-if="!locked && node.views.length > 1" class="node-tabs">
        <JvSegmented v-model="activeView" variant="tabs" :items="node.views" :label="`${node.label} views`" controls="node-view" />
      </div>
      <main id="node-view" :class="['page', `page-${active}`, { 'with-tabs': !locked && node.views.length > 1, 'logs-active': active === 'logs', 'locked-page': locked }]">
        <component :is="current" v-if="!locked" @restart-required="restartRequired = true" @open="(view: ViewName) => (active = view)" />
        <section v-else class="lock-screen" aria-live="polite">
          <JvOrb :size="190" tone="idle" still :label="false" />
          <JvPanel icon="shield" title="Jarvis Core is locked" tone="warn" status="Locked" description="Authenticate once through the GNOME system dialog. Your password is never handled by this application.">
            <div v-if="authError" class="lock-error">{{ authError }}</div>
            <div class="lock-actions">
              <button class="primary" :disabled="authBusy" @click="unlock">{{ authBusy ? "Waiting for system authorization…" : "Unlock administration" }}</button>
              <small>The session locks after five minutes without pointer or keyboard activity.</small>
            </div>
          </JvPanel>
        </section>
      </main>
    </div>
    <div v-if="restartRequired" class="dialog-backdrop restart-backdrop">
      <section class="dialog restart-dialog" role="alertdialog" aria-modal="true" aria-labelledby="restart-title">
        <span class="eyebrow">UPDATE INSTALLED</span>
        <h2 id="restart-title">Restart Jarvis Core Administration</h2>
        <p>The trusted update completed and replaced administration components. This older application process cannot continue safely.</p>
        <p v-if="restartError" class="restart-error">{{ restartError }}</p>
        <div class="dialog-actions"><button class="primary" :disabled="restartBusy" @click="restartNow">{{ restartBusy ? "Restarting…" : "Restart now" }}</button></div>
      </section>
    </div>
  </div>
</template>
