<script setup lang="ts">
import { computed, nextTick, onBeforeUnmount, onMounted, reactive, ref, watch } from "vue";
import { api, errorText, type ViewName } from "./admin";
import { dialogFocus } from "./focus";
import { IDLE_REASON, createSession, initialSession, lockTime, navigate, visibleView } from "./session";
import NavIcon, { type IconName } from "./components/NavIcon.vue";
import StatusBadge from "./components/StatusBadge.vue";
import ThemeSelect from "./components/ThemeSelect.vue";
import JvPanel from "./components/jv/JvPanel.vue";
import JvSegmented from "./components/jv/JvSegmented.vue";
import JvTopBar from "./components/jv/JvTopBar.vue";
import AgentsView from "./views/AgentsView.vue";
import CredentialsView from "./views/CredentialsView.vue";
import AiAccountsView from "./views/AiAccountsView.vue";
import DevicesView from "./views/DevicesView.vue";
import LogsView from "./views/LogsView.vue";
import ModelsView from "./views/ModelsView.vue";
import UsageView from "./views/UsageView.vue";
import OverviewView from "./views/OverviewView.vue";
import PersonaView from "./views/PersonaView.vue";
import SystemView from "./views/SystemView.vue";
import UpdateView from "./views/UpdateView.vue";

// One flat sidebar. An entry with `tabs` shows them as sub-tabs above its page.
type NavItem = { id: ViewName; label: string; icon: IconName; tabs?: { id: ViewName; label: string }[] };
const sections: { label?: string; items: NavItem[] }[] = [
  { items: [
    { id: "overview", label: "Overview", icon: "core" },
    { id: "devices", label: "Devices", icon: "monitor" },
    { id: "releases", label: "Releases", icon: "download" },
  ] },
  { label: "AI", items: [
    { id: "models", label: "Models & routing", icon: "chip" },
    // ponytail: two sub-tabs until AI accounts and credentials merge into one view.
    { id: "ai-accounts", label: "Accounts & keys", icon: "key", tabs: [{ id: "ai-accounts", label: "AI accounts" }, { id: "credentials", label: "Credentials" }] },
    { id: "usage", label: "Usage & costs", icon: "chart" },
    { id: "agents", label: "Agents", icon: "agents" },
    { id: "persona", label: "Persona", icon: "user" },
  ] },
  { label: "Diagnostics", items: [
    { id: "logs", label: "Logs", icon: "lines" },
    { id: "system", label: "System", icon: "gear" },
  ] },
];
const views = { overview: OverviewView, devices: DevicesView, releases: UpdateView, models: ModelsView, "ai-accounts": AiAccountsView, credentials: CredentialsView, usage: UsageView, agents: AgentsView, persona: PersonaView, logs: LogsView, system: SystemView };
const session = reactive(initialSession());
const { lock, unlock: authenticate, recordActivity, start, stop } = createSession(session, api, errorText);
const active = ref<ViewName>("overview");
// Reported by the Overview and Devices views from data they already load.
const pendingPairings = ref(0);
const restartRequired = ref(false);
const restartBusy = ref(false);
const restartError = ref("");
const page = ref<HTMLElement | null>(null);
const unlockButton = ref<HTMLButtonElement | null>(null);
const restartDialog = ref<HTMLElement | null>(null);
let restartFocus: ReturnType<typeof dialogFocus> | undefined;
const OWNER_REASON = "Locked by owner.";
// An idle or owner lock is expected; only real failures are alerts.
const lockNote = computed(() => [IDLE_REASON, OWNER_REASON].includes(session.authError));
const current = computed(() => visibleView(session.locked, views[active.value]));
const item = computed(() => sections.flatMap((section) => section.items).find((entry) => entry.id === active.value || entry.tabs?.some((tab) => tab.id === active.value))!);
const tabs = computed(() => (session.locked ? undefined : item.value.tabs));
const activeTab = computed({ get: () => active.value, set: (id: string) => go(id as ViewName) });
const locksAt = computed(() => lockTime(session.lastActivity));
let runtimeTimer: number | undefined;
// A lock or unlock removes the focused element; focus moves to what replaced it.
watch(() => session.locked, (locked) => {
  if (locked) pendingPairings.value = 0;
  // The restart dialog is modal; focus stays in it.
  if (!restartRequired.value) void nextTick(() => (locked ? unlockButton.value : page.value)?.focus());
});
// The restart gate has no Escape and no restore: it never closes.
watch(restartRequired, (required) => {
  if (!required) return;
  void nextTick(() => {
    restartFocus = dialogFocus(restartDialog.value!);
    restartDialog.value!.querySelector("button")?.focus();
  });
});
function go(view: ViewName) { active.value = navigate(session.locked, active.value, view); }
// A card that opens another view unmounts itself; focus moves to the new page.
function open(view: ViewName) {
  go(view);
  void nextTick(() => page.value?.focus());
}
function unlock() { return authenticate(checkRuntime); }
async function checkRuntime() {
  if (session.locked || restartRequired.value) return;
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
onMounted(() => {
  start();
  runtimeTimer = window.setInterval(() => void checkRuntime(), 15_000);
  void unlock();
});
onBeforeUnmount(() => {
  clearInterval(runtimeTimer);
  stop();
});
</script>
<template>
  <div class="app-shell" @pointermove="recordActivity" @pointerdown="recordActivity" @mouseenter="recordActivity" @wheel="recordActivity" @touchstart="recordActivity" @keydown="recordActivity" @focusin="recordActivity">
    <nav class="sidebar" aria-label="Core Admin">
      <template v-for="(section, s) in sections" :key="s">
        <p v-if="section.label" :id="`nav-section-${s}`" class="nav-section">{{ section.label }}</p>
        <ul :aria-labelledby="section.label ? `nav-section-${s}` : undefined">
          <li v-for="entry in section.items" :key="entry.id">
            <button type="button" class="nav-entry" :disabled="session.locked" :aria-current="!session.locked && entry === item ? 'page' : undefined" @click="go(entry.id)">
              <NavIcon :name="entry.icon" />
              <span class="nav-label">{{ entry.label }}</span>
              <span v-if="entry.id === 'devices' && !session.locked && pendingPairings" class="nav-badge">{{ pendingPairings }}<span class="sr-only"> pending pairing{{ pendingPairings === 1 ? "" : "s" }}</span></span>
            </button>
          </li>
        </ul>
      </template>
    </nav>
    <div class="main-shell">
      <JvTopBar :title="session.locked ? 'Locked' : undefined">
        <div class="session-cluster">
          <StatusBadge v-if="session.locked" tone="warn" label="Locked" />
          <StatusBadge v-else tone="warn" :label="`Root session · locks ${locksAt}`" />
          <ThemeSelect />
          <button v-if="!session.locked" class="small" @click="lock(OWNER_REASON)">Lock now</button>
        </div>
      </JvTopBar>
      <div v-if="tabs" class="node-tabs">
        <JvSegmented v-model="activeTab" variant="tabs" :items="tabs" :label="`${item.label} views`" controls="page-view" />
      </div>
      <main id="page-view" ref="page" tabindex="-1" :role="tabs ? 'tabpanel' : undefined" :class="['page', { 'with-tabs': tabs, 'logs-active': active === 'logs', 'locked-page': session.locked }]">
        <component :is="current" v-if="current" @restart-required="restartRequired = true" @open="open" @pairings="(count: number) => (pendingPairings = count)" />
        <section v-else class="lock-screen">
          <JvPanel icon="shield" title="Jarvis Core is locked" tone="warn" status="Locked" description="Authenticate once through the GNOME system dialog. Your password is never handled by this application.">
            <p v-if="lockNote" class="lock-note" role="status">{{ session.authError }}</p>
            <div v-else-if="session.authError" class="lock-error" role="alert">{{ session.authError }}</div>
            <div class="lock-actions">
              <button ref="unlockButton" class="primary" :disabled="session.authBusy" @click="unlock">{{ session.authBusy ? "Waiting for system authorization…" : "Unlock administration" }}</button>
              <small>The session locks after five minutes without pointer or keyboard activity.</small>
            </div>
          </JvPanel>
        </section>
      </main>
    </div>
    <div v-if="restartRequired" class="dialog-backdrop restart-backdrop">
      <section ref="restartDialog" class="dialog restart-dialog" role="alertdialog" aria-modal="true" tabindex="-1" aria-labelledby="restart-title" aria-describedby="restart-detail" @keydown="restartFocus?.keydown($event)">
        <span class="eyebrow">Update installed</span>
        <h2 id="restart-title">Restart Jarvis Core Administration</h2>
        <p id="restart-detail">The trusted update completed and replaced administration components. This older application process cannot continue safely.</p>
        <p v-if="restartError" class="restart-error">{{ restartError }}</p>
        <div class="dialog-actions"><button class="primary" :disabled="restartBusy" @click="restartNow">{{ restartBusy ? "Restarting…" : "Restart now" }}</button></div>
      </section>
    </div>
  </div>
</template>
