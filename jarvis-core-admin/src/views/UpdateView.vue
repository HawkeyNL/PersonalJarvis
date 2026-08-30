<script setup lang="ts">
import { onMounted, ref } from "vue";
import { api, errorText, type OperationResult } from "../admin";
import ConfirmDialog from "../components/ConfirmDialog.vue";
import ErrorPanel from "../components/ErrorPanel.vue";
import PageHeader from "../components/PageHeader.vue";
import ResultPanel from "../components/ResultPanel.vue";
import StatusBadge from "../components/StatusBadge.vue";
const status = ref<Record<string, string>>({}); const busy = ref(false); const error = ref(""); const result = ref<OperationResult | null>(null); const version = ref(""); const pending = ref<"latest" | "version" | "rollback" | null>(null);
async function load(check = false) { busy.value = true; error.value = ""; try { status.value = await api.updateStatus(check); } catch (e) { error.value = errorText(e); } finally { busy.value = false; } }
async function execute() { const action = pending.value; pending.value = null; if (!action) return; busy.value = true; error.value = ""; result.value = null; try { const request: Record<string, string> = { action: action === "version" ? "install_version" : action }; if (action === "version") request.version = version.value; result.value = await api.updateMutation(request); await load(false); } catch (e) { error.value = errorText(e); } finally { busy.value = false; } }
onMounted(() => load(false));
</script>
<template>
  <PageHeader title="Update Center" description="Release verification, staging, activation and rollback remain owned by the trusted Core updater." :busy="busy"><button class="secondary" @click="load(false)">Refresh</button><button class="secondary" @click="load(true)">Check for updates</button></PageHeader>
  <ErrorPanel v-if="error" :message="error" /><ResultPanel v-if="result" :result="result" />
  <section class="metric-grid update-metrics"><article v-for="(value, key) in status" :key="key" class="metric-card"><span class="card-label">{{ key }}</span><StatusBadge v-if="key === 'update' || key === 'updater'" :state="value" /><strong v-else>{{ value }}</strong></article></section>
  <section class="action-card"><div><h2>Install a verified release</h2><p>GNOME will request administrator authorization. No password enters this application.</p></div><div class="action-row"><button @click="pending = 'latest'">Update to latest</button><input v-model="version" placeholder="v0.0.17" aria-label="Specific version" /><button class="secondary" :disabled="!version" @click="pending = 'version'">Install version</button><button class="danger ghost" @click="pending = 'rollback'">Rollback</button></div></section>
  <ConfirmDialog v-if="pending" :title="pending === 'rollback' ? 'Roll back Core?' : 'Start trusted Core update?'" detail="The trusted updater will revalidate the target and owns the complete transaction. Closing this window does not weaken updater policy." :confirm-label="pending === 'rollback' ? 'Confirm rollback' : 'Continue'" @cancel="pending = null" @confirm="execute" />
</template>
