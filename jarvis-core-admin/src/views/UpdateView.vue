<script setup lang="ts">
import { computed, onMounted, ref } from "vue";
import { api, errorText, type OperationResult } from "../admin";
import ConfirmDialog from "../components/ConfirmDialog.vue";
import ErrorPanel from "../components/ErrorPanel.vue";
import PageHeader from "../components/PageHeader.vue";
import ResultPanel from "../components/ResultPanel.vue";
import StatusBadge from "../components/StatusBadge.vue";
import { fieldLabel, keyedState, stateLabel } from "../status";
const emit = defineEmits<{ restartRequired: [] }>();
const status = ref<Record<string, string>>({}); const busy = ref(false); const error = ref(""); const errorTitle = ref<string>(); const result = ref<OperationResult | null>(null); const version = ref(""); const pending = ref<"latest" | "version" | "rollback" | "migrate" | null>(null);
const migrateTarget = computed(() => (status.value.schema === "migration required" ? status.value.latest ?? "" : ""));
// A version the status does not report (not checked yet, or "unavailable") is left out of the label.
const known = (value?: string) => (value && value !== "unavailable" ? value : "");
const DETAIL = "The trusted updater will revalidate the target and owns the complete transaction. Closing this window does not weaken updater policy.";
const dialog = computed(() => {
  if (pending.value === "migrate") return { title: `Migrate Core to ${migrateTarget.value}?`, detail: "Core and SurrealDB stop and clients disconnect. A cold snapshot goes to /var/backups/jarvis-migrations before the database is migrated; on failure the snapshot is restored and the previous release restarted. Afterwards a binary-only rollback across this schema change is refused. GNOME asks for the administrator password again for this step.", confirmLabel: `Migrate to ${migrateTarget.value}`, tone: "danger" as const, requireText: migrateTarget.value };
  if (pending.value === "rollback") {
    const previous = known(status.value.previous);
    return { title: previous ? `Roll back Core to ${previous}?` : "Roll back Core?", detail: DETAIL, confirmLabel: previous ? `Roll back to ${previous}` : "Roll back Core", tone: "danger" as const, requireText: undefined };
  }
  const target = pending.value === "version" ? version.value : known(status.value.latest);
  return { title: target ? `Install Core ${target}?` : "Update Core to the latest release?", detail: DETAIL, confirmLabel: target ? `Install ${target}` : "Update to latest", tone: "primary" as const, requireText: undefined };
});
async function load(check = false) { busy.value = true; error.value = ""; try { status.value = await api.updateStatus(check); } catch (e) { error.value = errorText(e); errorTitle.value = "Could not load release status"; } finally { busy.value = false; } }
async function execute() { const action = pending.value; pending.value = null; if (!action) return; busy.value = true; error.value = ""; result.value = null; try { const request: Record<string, string> = { action: action === "version" ? "install_version" : action }; if (action === "version") request.version = version.value; if (action === "migrate") request.version = migrateTarget.value; result.value = await api.updateMutation(request); if (result.value.success) emit("restartRequired"); } catch (e) { error.value = errorText(e); errorTitle.value = undefined; } finally { busy.value = false; } }
onMounted(() => load(false));
</script>
<template>
  <PageHeader title="Releases" description="Release verification, staging, activation and rollback remain owned by the trusted Core updater." :busy="busy"><button class="secondary" @click="load(false)">Refresh</button><button class="secondary" @click="load(true)">Check for updates</button></PageHeader>
  <ErrorPanel v-if="error" :message="error" :title="errorTitle" /><ResultPanel v-if="result" :result="result" />
  <section class="metric-grid update-metrics"><article v-for="(value, key) in status" :key="key" class="metric-card"><span class="card-label">{{ fieldLabel(key) }}</span><StatusBadge v-if="key === 'update' || key === 'updater'" :state="keyedState(key, value)" /><strong v-else>{{ stateLabel(value) }}</strong></article></section>
  <section class="action-card"><div><h2>Install a verified release</h2><p>GNOME will request administrator authorization. No password enters this application.</p></div><div class="action-row"><button @click="pending = 'latest'">Update to latest</button><input v-model="version" placeholder="v0.0.17" aria-label="Specific version" /><button class="secondary" :disabled="!version" @click="pending = 'version'">Install version</button><button class="danger ghost" @click="pending = 'rollback'">Rollback</button></div></section>
  <section v-if="migrateTarget" class="action-card"><div><h2>Schema migration required</h2><p>{{ migrateTarget }} changes the database schema. Routine updates refuse it; migrate explicitly with a fresh administrator authorization.</p></div><div class="action-row"><button class="danger" :disabled="busy" @click="pending = 'migrate'">Migrate to {{ migrateTarget }}</button></div></section>
  <ConfirmDialog v-if="pending" :title="dialog.title" :detail="dialog.detail" :confirm-label="dialog.confirmLabel" :tone="dialog.tone" :require-text="dialog.requireText" @cancel="pending = null" @confirm="execute" />
</template>
