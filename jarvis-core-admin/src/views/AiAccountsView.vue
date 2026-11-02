<script setup lang="ts">
import { onMounted, ref } from "vue";
import { aiAccountLabels as labels, api, errorText, type AiAccountAction, type AiAccountProvider, type AiAccountRecord, type OperationResult, type ViewName } from "../admin";
import ConfirmDialog from "../components/ConfirmDialog.vue";
import ErrorPanel from "../components/ErrorPanel.vue";
import PageHeader from "../components/PageHeader.vue";
import ResultPanel from "../components/ResultPanel.vue";
import StatusBadge from "../components/StatusBadge.vue";
import { stateLabel } from "../status";

const rows = ref<AiAccountRecord[]>([]);
const busy = ref(false);
const error = ref("");
const errorTitle = ref<string>();
const result = ref<OperationResult | null>(null);
const confirmDisconnect = ref<AiAccountProvider | null>(null);
const emit = defineEmits<{ open: [view: ViewName] }>();

async function load() {
  busy.value = true;
  error.value = "";
  try { rows.value = await api.aiAccounts(); }
  catch (e) { error.value = errorText(e); errorTitle.value = "Could not load AI accounts"; }
  busy.value = false;
}

async function act(provider: AiAccountProvider, action: AiAccountAction) {
  confirmDisconnect.value = null;
  busy.value = true;
  error.value = "";
  result.value = null;
  try {
    result.value = await api.aiAccountAction(provider, action);
    rows.value = await api.aiAccounts();
  } catch (e) { error.value = errorText(e); errorTitle.value = undefined; }
  finally { busy.value = false; }
}

onMounted(load);
</script>

<template>
  <PageHeader title="AI accounts" description="Owner-only subscription connections. Provider credentials stay in dedicated worker storage, never in Core Admin or Jarvis chat." :busy="busy">
    <button class="secondary" :disabled="busy" @click="load">Check status</button>
  </PageHeader>
  <ErrorPanel v-if="error" :message="error" :title="errorTitle" />
  <ResultPanel v-if="result" :result="result" />
  <section class="credential-grid">
    <article v-for="row in rows" :key="row.provider" class="metric-card credential-card">
      <span class="card-label">{{ labels[row.provider] }}</span>
      <StatusBadge :state="row.state" />
      <p>Worker: {{ row.worker }}<br>Billing: {{ stateLabel(row.billing) }}<br>Runtime: {{ stateLabel(row.runtime) }}</p>
      <p v-if="row.state === 'incompatible_runtime'">The installed Claude CLI is outside the reviewed version contract. Recheck after an owner-reviewed CLI update before enabling the worker socket.</p>
      <p v-if="row.state === 'host_unsupported'">A host system tool the worker runs through (systemd-run or env) failed the root-ownership check. Connect reports which one.</p>
      <p>Runtime install, update and rollback: <button class="small ghost" @click="emit('open', 'releases')">Open Releases</button></p>
      <p v-if="row.legacy_identity">The retired <code>jarvis-codex-chat</code> identity or <code>/var/lib/jarvis-codex-chat</code> still exists. It is no longer used and can be removed (see <code>docs/SUBSCRIPTION_WORKERS.md</code>).</p>
      <p v-if="row.provider === 'claude' && row.state === 'connected'">Claude login is linked. Verify extra usage is disabled or capped at zero in your provider account before explicitly enabling the worker socket.</p>
      <div class="dialog-actions">
        <button v-if="row.state !== 'connected'" class="small secondary" :disabled="busy" @click="act(row.provider, 'connect')">Connect</button>
        <template v-else>
          <button class="small secondary" :disabled="busy" @click="act(row.provider, 'test')">Test</button>
          <button class="small secondary" :disabled="busy" @click="act(row.provider, 'reconnect')">Reconnect</button>
          <button class="small danger" :disabled="busy" @click="confirmDisconnect = row.provider">Disconnect…</button>
        </template>
      </div>
    </article>
    <article class="security-card"><strong>Separate trust boundary</strong><p>Connect and disconnect open a trusted terminal and require system administrator authorization. The official provider client handles browser authentication. No token or provider cookie enters this webview, Jarvis Core or its database. A connected login does not by itself enable coding runs or model access.</p></article>
  </section>
  <ConfirmDialog v-if="confirmDisconnect" :title="`Disconnect ${labels[confirmDisconnect]}?`" :detail="`${confirmDisconnect === 'codex' ? 'Also disables the Codex chat worker socket, which uses this login. ' : ''}Active runs may fail. Reconnecting opens a trusted terminal and requires system administrator authorization again.`" :confirm-label="`Disconnect ${labels[confirmDisconnect]}`" tone="danger" @cancel="confirmDisconnect = null" @confirm="act(confirmDisconnect, 'disconnect')" />
</template>
