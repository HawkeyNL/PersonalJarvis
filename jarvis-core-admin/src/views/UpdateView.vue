<script setup lang="ts">
import { computed, onBeforeUnmount, onMounted, ref } from "vue";
import { api, errorText } from "../admin";
import ConfirmDialog from "../components/ConfirmDialog.vue";
import ErrorPanel from "../components/ErrorPanel.vue";
import PageHeader from "../components/PageHeader.vue";
import ResultPanel from "../components/ResultPanel.vue";
import StatusBadge from "../components/StatusBadge.vue";
import RuntimeComponents from "../components/releases/RuntimeComponents.vue";
import { operations } from "../components/releases/operations";
import { DOWNTIME, cliCommand, isRunning, operationView, releaseHeadline, releaseOffer, releaseTag, track } from "../components/releases/release";
import { updaterLabel } from "../components/overview/needs";
import { fieldLabel, keyedState, stateLabel } from "../status";
const emit = defineEmits<{ restartRequired: [] }>();
const status = ref<Record<string, string>>({}); const busy = ref(false); const error = ref(""); const errorTitle = ref<string>(); const version = ref(""); const pending = ref<"version" | "repair" | "rollback" | "migrate" | null>(null);
// The version a confirmed install sends: the offered release or the one typed below.
const target = ref("");
const offer = computed(() => releaseOffer(status.value));
const running = computed(() => isRunning(operations));
const op = computed(() => operations.current && { ...operationView(operations.current), result: operations.current.result, error: operations.current.error });
const typedVersion = computed(() => releaseTag(version.value.trim()));
const hint = computed(() => cliCommand(offer.value));
const current = computed(() => offer.value.current || "the release that is active now");
// A broker request holds the session until it ends, so unlocking waits for it.
const LOCK_NOTE = "This can take up to 30 minutes. If the session locks meanwhile, the operation keeps running, unlocking waits until it has finished, and the outcome shows on this page.";
const dialog = computed(() => {
  const migrate = offer.value.migrate;
  if (pending.value === "migrate") return { title: `Migrate Core to ${migrate}?`, detail: `Core and SurrealDB stop and clients disconnect. A cold snapshot goes to /var/backups/jarvis-migrations before the database is migrated; on failure the snapshot is restored and the previous release restarted. Afterwards a binary-only rollback across this schema change is refused. GNOME asks for the administrator password again for this step. If the session locks meanwhile, the migration keeps running and the outcome shows on this page.`, confirmLabel: `Migrate to ${migrate}`, tone: "danger" as const, requireText: migrate };
  if (pending.value === "rollback") {
    const previous = offer.value.previous;
    return { title: previous ? `Roll back Core to ${previous}?` : "Roll back Core?", detail: `${DOWNTIME} The trusted updater activates ${previous || "the newest other verified release with the same schema"} in place of ${current.value}. If it fails its readiness check, the updater restores ${current.value} and restarts it. The database is not changed; a rollback across a schema migration is refused. ${LOCK_NOTE}`, confirmLabel: previous ? `Roll back to ${previous}` : "Roll back Core", tone: "danger" as const, requireText: undefined };
  }
  if (pending.value === "repair") return { title: `Repair Core ${target.value}?`, detail: `The trusted updater reinstalls the managed systemd units of ${target.value} and restarts the services. ${DOWNTIME} If the repair fails, the updater restores the previous unit policy. ${LOCK_NOTE}`, confirmLabel: `Repair ${target.value}`, tone: "primary" as const, requireText: undefined };
  return { title: `Install Core ${target.value}?`, detail: `${DOWNTIME} The trusted updater checks ${target.value} against its published SHA-256, stages it next to ${current.value} and activates it. If ${target.value} fails its readiness check, the updater switches back to ${current.value} and restarts it. A release that changes the database schema is refused here and needs Migrate. ${LOCK_NOTE}`, confirmLabel: `Install ${target.value}`, tone: "primary" as const, requireText: undefined };
});
async function load(check = false) { busy.value = true; error.value = ""; try { const values = await api.updateStatus(check); status.value = check ? { ...status.value, ...values } : values; } catch (e) { error.value = errorText(e); errorTitle.value = "Could not load release status"; } finally { busy.value = false; } }
function install(tag: string, action: "version" | "repair" = "version") { target.value = tag; pending.value = action; }
const LABELS = {
  version: () => ({ doing: `Installing Core ${target.value}`, done: `Install Core ${target.value}` }),
  repair: () => ({ doing: `Repairing Core ${target.value}`, done: `Repair Core ${target.value}` }),
  rollback: () => ({ doing: offer.value.previous ? `Rolling back Core to ${offer.value.previous}` : "Rolling back Core", done: offer.value.previous ? `Roll back Core to ${offer.value.previous}` : "Roll back Core" }),
  migrate: () => ({ doing: `Migrating Core to ${offer.value.migrate}`, done: `Migrate Core to ${offer.value.migrate}` }),
};
async function execute() {
  const action = pending.value; pending.value = null; if (!action) return;
  error.value = "";
  // A repair is `install_version` with the active tag, as `sudo jarvis update --version` is.
  const request: Record<string, string> = { action: action === "version" || action === "repair" ? "install_version" : action };
  if (action === "version" || action === "repair") request.version = target.value;
  if (action === "migrate") request.version = offer.value.migrate;
  const result = await track(operations, { ...LABELS[action](), note: "The trusted updater owns the transaction and finishes it even if this session locks." }, () => api.updateMutation(request), errorText);
  // Unmounted by a lock or a page change: the emit is dropped and the shell's
  // runtime check raises the restart gate instead, as before.
  if (result?.success) emit("restartRequired");
  if (mounted) void load(false);
}
const copied = ref("");
let mounted = false;
let copyTimer: number | undefined;
async function copy() {
  try { await navigator.clipboard.writeText(hint.value); copied.value = "Copied"; }
  catch { copied.value = "Copy is blocked here; select the command instead."; }
  clearTimeout(copyTimer); copyTimer = window.setTimeout(() => (copied.value = ""), 2000);
}
// Re-read on every mount: after an unlock or a page change this shows the
// release that an operation started earlier left active.
onMounted(() => { mounted = true; void load(false); });
onBeforeUnmount(() => { mounted = false; clearTimeout(copyTimer); });
</script>
<template>
  <PageHeader title="Releases" description="Release verification, staging, activation and rollback remain owned by the trusted Core updater." :busy="busy || running"><button class="secondary" @click="load(false)">Refresh</button><button class="secondary" @click="load(true)">Check for updates</button></PageHeader>
  <ErrorPanel v-if="error" :message="error" :title="errorTitle" />
  <!-- Always rendered, so the start is announced; the outcome is announced by the result or error panel. -->
  <p class="sr-only" role="status">{{ op?.phase === "running" ? `${op.headline}. ${op.detail}` : "" }}</p>
  <section v-if="op" class="card op-card">
    <p class="op-head"><span v-if="op.phase === 'running'" class="spinner" aria-hidden="true"></span><StatusBadge :tone="op.phase === 'running' ? 'idle' : op.phase === 'done' ? 'ok' : 'error'" :label="op.badge" /><strong>{{ op.headline }}</strong></p>
    <p class="op-detail">{{ op.detail }}</p>
  </section>
  <ResultPanel v-if="op?.result" :result="op.result" /><ErrorPanel v-if="op?.error" :message="op.error" />
  <section class="card release-card" aria-labelledby="core-title">
    <span class="eyebrow">Jarvis Core</span>
    <h2 id="core-title">{{ releaseHeadline(offer) }}</h2>
    <p class="release-sub">Installed {{ offer.current || "unknown" }} · previous {{ offer.previous || "none" }}<template v-if="offer.migrate"> · routine updates refuse this release; migrate explicitly with a fresh administrator authorization.</template></p>
    <div class="action-row">
      <button v-if="offer.install" class="primary" :disabled="busy || running" @click="install(offer.install)">Install {{ offer.install }}</button>
      <button v-if="offer.repair" class="primary" :disabled="busy || running" @click="install(offer.repair, 'repair')">Repair {{ offer.repair }}</button>
      <button v-if="offer.migrate" class="danger" :disabled="busy || running" @click="pending = 'migrate'">Migrate to {{ offer.migrate }}</button>
      <button class="danger ghost" :disabled="busy || running" @click="pending = 'rollback'">{{ offer.previous ? `Roll back to ${offer.previous}` : "Roll back" }}</button>
    </div>
    <div v-if="hint" class="cli-hint">
      <span class="card-label">Or in a terminal on this Home Node</span>
      <div class="cli-row"><code class="command">{{ hint }}</code><button class="small secondary" @click="copy">Copy</button></div>
      <span class="t-xs" role="status">{{ copied }}</span>
    </div>
    <details class="specific">
      <summary>Install a specific version</summary>
      <div class="action-row"><input v-model="version" placeholder="v0.0.17" aria-label="Specific version" maxlength="32" /><button class="secondary" :disabled="!typedVersion || busy || running" @click="install(typedVersion)">Install version</button></div>
    </details>
  </section>
  <section class="metric-grid update-metrics" aria-label="Release details"><article v-for="(value, key) in status" :key="key" class="metric-card"><span class="card-label">{{ fieldLabel(key) }}</span><StatusBadge v-if="key === 'updater' && updaterLabel(value)" tone="idle" :label="updaterLabel(value)!" /><StatusBadge v-else-if="key === 'update' || key === 'updater'" :state="keyedState(key, value)" /><strong v-else>{{ stateLabel(value) }}</strong></article></section>
  <RuntimeComponents :disabled="busy" />
  <ConfirmDialog v-if="pending" :title="dialog.title" :detail="dialog.detail" :confirm-label="dialog.confirmLabel" :tone="dialog.tone" :require-text="dialog.requireText" @cancel="pending = null" @confirm="execute" />
</template>
<style scoped>
.op-card, .release-card { margin-bottom: 16px; }
.op-head { display: flex; align-items: center; gap: 10px; margin: 0; overflow-wrap: anywhere; }
.op-head strong { font-weight: 600; }
.op-detail, .release-sub { margin: 6px 0 0; color: var(--text-secondary); font-size: 13px; line-height: 1.5; overflow-wrap: anywhere; }
.release-card h2 { margin: 4px 0 0; font-size: 22px; overflow-wrap: anywhere; }
.release-card .action-row { margin-top: 16px; }
.cli-hint { display: flex; flex-direction: column; gap: 6px; margin-top: 16px; }
.cli-hint .t-xs { min-height: 1.4em; color: var(--text-secondary); }
.cli-row { display: flex; align-items: center; gap: 8px; }
.command { flex: 1; min-width: 0; padding: 10px 12px; border-radius: var(--r-md); background: var(--bg-sunken); font: 13px/1.5 var(--font-mono); overflow-wrap: anywhere; user-select: all; }
.specific { margin-top: 16px; }
.specific summary { width: fit-content; color: var(--text-secondary); font-size: 13px; cursor: pointer; }
.specific .action-row { margin-top: 10px; }
</style>
