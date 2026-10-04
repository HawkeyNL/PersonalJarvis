<script setup lang="ts">
import { onMounted, ref } from "vue";
import { api, errorText, type AiAccountAction, type AiAccountProvider, type AiAccountRecord, type OperationResult } from "../admin";
import ErrorPanel from "../components/ErrorPanel.vue";
import PageHeader from "../components/PageHeader.vue";
import ResultPanel from "../components/ResultPanel.vue";
import StatusBadge from "../components/StatusBadge.vue";

const rows = ref<AiAccountRecord[]>([]);
const busy = ref(false);
const error = ref("");
const result = ref<OperationResult | null>(null);
const labels: Record<AiAccountProvider, string> = { claude: "Claude Code", codex: "Codex (coding)", "codex-chat": "Codex chat" };

async function load() {
  busy.value = true;
  error.value = "";
  try { rows.value = await api.aiAccounts(); }
  catch (e) { error.value = errorText(e); }
  finally { busy.value = false; }
}

async function act(provider: AiAccountProvider, action: AiAccountAction) {
  if (action === "disconnect" && !window.confirm(`Disconnect the ${provider} subscription? Active runs may fail.`)) return;
  busy.value = true;
  error.value = "";
  result.value = null;
  try {
    result.value = await api.aiAccountAction(provider, action);
    rows.value = await api.aiAccounts();
  } catch (e) { error.value = errorText(e); }
  finally { busy.value = false; }
}

onMounted(load);
</script>

<template>
  <PageHeader title="AI Accounts" description="Owner-only subscription connections. Provider credentials stay in dedicated worker storage, never in Core Admin or Jarvis chat." :busy="busy">
    <button class="secondary" :disabled="busy" @click="load">Check status</button>
  </PageHeader>
  <ErrorPanel v-if="error" :message="error" />
  <ResultPanel v-if="result" :result="result" />
  <section class="credential-grid">
    <article v-for="row in rows" :key="row.provider" class="metric-card credential-card">
      <span class="card-label">{{ labels[row.provider] }}</span>
      <StatusBadge :state="row.state" />
      <p>Worker: {{ row.worker }}<br>Billing: {{ row.billing === "overage_unverified" ? "Extra usage setting not verified" : row.billing }}<br>Runtime: {{ row.runtime }}</p>
      <p v-if="row.provider === 'claude' && row.state === 'connected'">Claude login is linked. Verify extra usage is disabled or capped at zero in your provider account before explicitly enabling the worker socket.</p>
      <div class="dialog-actions">
        <button v-if="row.state !== 'connected'" class="small secondary" :disabled="busy" @click="act(row.provider, 'connect')">Connect</button>
        <template v-else>
          <button class="small secondary" :disabled="busy" @click="act(row.provider, 'test')">Test</button>
          <button class="small secondary" :disabled="busy" @click="act(row.provider, 'reconnect')">Reconnect</button>
          <button class="small secondary" :disabled="busy" @click="act(row.provider, 'disconnect')">Disconnect</button>
        </template>
      </div>
    </article>
    <article class="security-card"><strong>Separate trust boundary</strong><p>Connect and disconnect open a trusted terminal and require system administrator authorization. The official provider client handles browser authentication. No token or provider cookie enters this webview, Jarvis Core or its database. A connected login does not by itself enable coding runs or model access.</p></article>
  </section>
</template>
