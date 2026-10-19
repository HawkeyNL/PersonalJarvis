<script setup lang="ts">
import { computed, onMounted, ref } from "vue";
import { api, errorText, type OperationResult, type PersonaMutation, type PersonaStatus } from "../admin";
import { installBlocker, problemLabel, shortHash } from "../persona";
import ConfirmDialog from "../components/ConfirmDialog.vue";
import ErrorPanel from "../components/ErrorPanel.vue";
import JvStatusDot from "../components/jv/JvStatusDot.vue";
import PageHeader from "../components/PageHeader.vue";
import ResultPanel from "../components/ResultPanel.vue";
const data = ref<PersonaStatus | null>(null); const busy = ref(false); const error = ref(""); const result = ref<OperationResult | null>(null);
const pending = ref<{ request: PersonaMutation; title: string; detail: string } | null>(null);
const blocker = computed(() => (data.value ? installBlocker(data.value) : "Loading persona status."));
const signatures = { verified: "Signed by an allowed signer", untrusted: "Not signed by an allowed signer", no_allowed_signers: "No allowed signers configured" };
const time = (seconds: number) => new Date(seconds * 1000).toLocaleString();
// After a mutation, keep its error visible while the status refreshes.
async function load(clear = true) { busy.value = true; if (clear) error.value = ""; try { data.value = await api.persona(); } catch (e) { if (clear || !error.value) error.value = errorText(e); } finally { busy.value = false; } }
function askInstall() {
  const hash = data.value?.candidate?.sha256;
  if (!hash || blocker.value) return;
  pending.value = { request: { action: "install", expect_sha256: hash }, title: `Install persona ${shortHash(hash)}?`, detail: `Replaces the current persona with the candidate SHA-256 ${hash} and restarts Core. Nothing is installed unless the signed checkout still has exactly this hash; if Core does not become ready, the previous persona is restored.` };
}
function askRollback(hash: string) {
  pending.value = { request: { action: "rollback", to_sha256: hash }, title: `Roll back to persona ${shortHash(hash)}?`, detail: `Replaces the current persona with history version ${hash} and restarts Core. If Core does not become ready, the previous persona is restored.` };
}
async function execute() {
  const selected = pending.value; pending.value = null;
  if (!selected) return;
  busy.value = true; error.value = ""; result.value = null;
  try { result.value = await api.personaMutation(selected.request); } catch (e) { error.value = errorText(e); }
  await load(false);
}
onMounted(() => load());
</script>
<template>
  <PageHeader title="Persona" description="Jarvis' protected persona. Only hashes, sizes and times are shown; persona text is never loaded by this application." :busy="busy"><button class="secondary" @click="load()">Refresh</button></PageHeader>
  <ErrorPanel v-if="error" :message="error" /><ResultPanel v-if="result" :result="result" />
  <template v-if="data">
    <section class="metric-grid persona-metrics">
      <article class="metric-card">
        <span class="card-label">INSTALLED</span>
        <template v-if="data.installed"><strong class="mono">{{ shortHash(data.installed.sha256) }}</strong><span class="persona-meta">{{ data.installed.bytes }} bytes · changed {{ time(data.installed.modified) }}</span><JvStatusDot :tone="data.installed.problem ? 'error' : 'ok'" :label="problemLabel(data.installed.problem)" :size="7" /></template>
        <strong v-else>None</strong>
      </article>
      <article class="metric-card">
        <span class="card-label">CANDIDATE</span>
        <template v-if="data.candidate"><strong class="mono">{{ data.candidate.sha256 ? shortHash(data.candidate.sha256) : "too large" }}</strong><span class="persona-meta">commit <span class="mono">{{ data.candidate.commit }}</span><template v-if="data.candidate.bytes !== null"> · {{ data.candidate.bytes }} bytes</template></span><JvStatusDot :tone="data.candidate.signature === 'verified' ? 'ok' : 'error'" :label="signatures[data.candidate.signature]" :size="7" /><JvStatusDot :tone="data.candidate.problem ? 'error' : 'ok'" :label="problemLabel(data.candidate.problem)" :size="7" /></template>
        <template v-else><strong>Unavailable</strong><span class="persona-meta wrap-anywhere">{{ data.candidate_error ?? "No candidate" }}</span></template>
      </article>
    </section>
    <section class="action-card">
      <div><h2>Install candidate</h2><p>{{ blocker ?? `Install ${shortHash(data.candidate?.sha256 ?? "")} from the signed private checkout. The trusted admin CLI installs exactly this hash, restarts Core and checks readiness.` }}</p></div>
      <div class="action-row"><button :disabled="busy || !!blocker" @click="askInstall">Install candidate…</button></div>
    </section>
    <section class="table-card">
      <table><thead><tr><th>History version</th><th class="num">Size</th><th>Last used</th><th>State</th><th></th></tr></thead>
        <tbody><tr v-for="entry in data.history" :key="entry.sha256"><td class="mono">{{ shortHash(entry.sha256) }}</td><td class="num">{{ entry.bytes }} bytes</td><td>{{ time(entry.modified) }}</td><td><JvStatusDot :tone="entry.problem ? 'error' : entry.installed ? 'ok' : 'idle'" :label="entry.problem ? problemLabel(entry.problem) : entry.installed ? 'Installed' : 'Available'" :size="7" /></td><td class="table-actions"><button class="small danger ghost" :disabled="busy || entry.installed || entry.problem !== null" @click="askRollback(entry.sha256)">Roll back…</button></td></tr></tbody>
      </table>
      <div v-if="!data.history.length" class="empty-state">No persona history yet.</div>
    </section>
    <p class="security-note">Edit the persona through signed commits in the private repository; this application has no editor.</p>
  </template>
  <ConfirmDialog v-if="pending" :title="pending.title" :detail="pending.detail" confirm-label="Continue" @cancel="pending = null" @confirm="execute" />
</template>
<style scoped>
.persona-metrics { margin-bottom: 14px; }
.persona-meta { color: var(--text-4); font-size: var(--fs-13); }
</style>
