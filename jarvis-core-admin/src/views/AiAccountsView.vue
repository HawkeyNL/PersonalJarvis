<script setup lang="ts">
import { nextTick, onMounted, ref } from "vue";
import {
  aiAccountLabels as labels, api, credentialLabels, errorText,
  type AiAccountAction, type AiAccountProvider, type AiAccountRecord, type CredentialProvider, type CredentialRecord, type ModelRecord, type OperationResult, type RoutingReport, type ViewName,
} from "../admin";
import ConfirmDialog from "../components/ConfirmDialog.vue";
import ErrorPanel from "../components/ErrorPanel.vue";
import PageHeader from "../components/PageHeader.vue";
import ResultPanel from "../components/ResultPanel.vue";
import StatusBadge from "../components/StatusBadge.vue";
import { accountConfirm, keyConfirm, keyUseLabel, type ConfirmSpec } from "../components/accounts/accounts";
import { keyInUse, keyUse } from "../components/overview/needs";
import { keyedState, stateLabel } from "../status";

const accounts = ref<AiAccountRecord[]>([]);
const keys = ref<CredentialRecord[]>([]);
// Null until both arrived: key usage is unknown, never "unused", without them.
const usage = ref<{ models: ModelRecord[]; routes: RoutingReport } | null>(null);
const errors = ref({ accounts: "", keys: "", usage: "" });
const busy = ref(false);
const actionError = ref("");
const result = ref<OperationResult | null>(null);
// The button whose trusted terminal is open, e.g. "key:openai", and what it is for.
const running = ref("");
const runningFor = ref("");
const confirm = ref<{ spec: ConfirmSpec; run: () => void } | null>(null);
const emit = defineEmits<{ open: [view: ViewName] }>();

async function settle<T>(read: () => Promise<T>, name: keyof typeof errors.value): Promise<T | null> {
  try {
    const data = await read();
    errors.value[name] = "";
    return data;
  } catch (e) {
    errors.value[name] = errorText(e);
    return null;
  }
}
const loadAccounts = async () => { accounts.value = (await settle(api.aiAccounts, "accounts")) ?? accounts.value; };
const loadKeys = async () => { keys.value = (await settle(api.credentials, "keys")) ?? keys.value; };
const loadUsage = async () => { usage.value = await settle(async () => {
  const [models, routes] = await Promise.all([api.models(), api.modelRoutes()]);
  return { models, routes };
}, "usage"); };
async function load() {
  busy.value = true;
  actionError.value = "";
  result.value = null;
  await Promise.all([loadAccounts(), loadKeys(), loadUsage()]);
  busy.value = false;
}

// The clicked button is disabled while its terminal is open, which drops focus
// (a confirm dialog gives it to the page). Afterwards the button gets it back,
// or the page when the button is gone (Connect becomes Test). Only when focus
// was lost: the owner may have moved on.
async function returnFocus(opener: Element | null) {
  const page = opener?.closest<HTMLElement>("[tabindex='-1']");
  await nextTick();
  const active = document.activeElement;
  if (active && active !== document.body && active !== page) return;
  if (opener instanceof HTMLElement && opener.isConnected && !opener.matches(":disabled")) opener.focus();
  else page?.focus();
}

// Every action runs in a trusted terminal; the broker call returns when it closes.
async function run(id: string, name: string, operation: () => Promise<OperationResult>, reload: () => Promise<void>, opener: Element | null) {
  confirm.value = null;
  busy.value = true;
  running.value = id;
  runningFor.value = name;
  actionError.value = "";
  result.value = null;
  try { result.value = await operation(); }
  catch (e) { actionError.value = errorText(e); }
  finally {
    // The operation may have succeeded even when this refresh fails.
    await reload();
    busy.value = false;
    running.value = "";
    runningFor.value = "";
    void returnFocus(opener);
  }
}
function account(provider: AiAccountProvider, action: AiAccountAction) {
  const opener = document.activeElement;
  const go = () => run(`${action}:${provider}`, labels[provider], () => api.aiAccountAction(provider, action), loadAccounts, opener);
  const spec = accountConfirm(provider, labels[provider], action);
  if (spec) confirm.value = { spec, run: go };
  else void go();
}
function setKey(row: CredentialRecord) {
  const opener = document.activeElement;
  const go = () => run(`key:${row.provider}`, `the ${credentialLabels[row.provider]} key`, () => api.credentialSet(row.provider), loadKeys, opener);
  const spec = keyConfirm(credentialLabels[row.provider], row.configured);
  if (spec) confirm.value = { spec, run: go };
  else void go();
}
const useOf = (provider: CredentialProvider) => (usage.value ? keyUse(provider, usage.value.models, usage.value.routes) : undefined);
const needed = (row: CredentialRecord) => !row.configured && keyInUse(useOf(row.provider) ?? null);
const label = (id: string, idle: string) => (running.value === id ? "Terminal open…" : idle);

onMounted(load);
</script>

<template>
  <PageHeader title="Accounts &amp; keys" description="Subscription logins and API keys for model providers. This app shows their status only; logins and keys are entered in a trusted terminal, never here." :busy="busy">
    <button class="secondary" @click="load">Refresh</button>
  </PageHeader>
  <p class="sr-only" role="status">{{ runningFor ? `A trusted terminal window is open for ${runningFor}. Finish there; this page updates when it closes.` : "" }}</p>
  <ErrorPanel v-if="actionError" :message="actionError" />
  <ResultPanel v-if="result" :result="result" />

  <section class="accounts-section" aria-labelledby="subscriptions-title">
    <h2 id="subscriptions-title">Subscriptions</h2>
    <p id="subscriptions-hint" class="section-hint">Connect, Test, Reconnect and Disconnect open a trusted terminal window and ask for system administrator authorization. The official provider client signs in there, inside its own worker identity, so no login, token or cookie passes through this app, Jarvis Core or its database. A connected login does not by itself enable coding runs or model access.</p>
    <ErrorPanel v-if="errors.accounts" title="Could not load subscriptions" :message="errors.accounts" />
    <div class="credential-grid">
      <article v-for="row in accounts" :key="row.provider" class="metric-card credential-card" :aria-labelledby="`account-${row.provider}`">
        <h3 :id="`account-${row.provider}`" class="card-label">{{ labels[row.provider] }}</h3>
        <StatusBadge :state="row.state" />
        <p>Billing: {{ stateLabel(keyedState("billing", row.billing)) }}<br>Runtime: {{ stateLabel(keyedState("runtime", row.runtime)) }}<br>Worker: <code>{{ row.worker }}</code></p>
        <p v-if="row.state === 'incompatible_runtime'">The installed Claude CLI is outside the reviewed version contract. Recheck after an owner-reviewed CLI update before enabling the worker socket.</p>
        <p v-if="row.state === 'host_unsupported'">A host system tool the worker runs through (systemd-run or env) failed the root-ownership check. Connect reports which one.</p>
        <p v-if="row.legacy_identity">The retired <code>jarvis-codex-chat</code> identity or <code>/var/lib/jarvis-codex-chat</code> still exists. It is no longer used and can be removed (see <code>docs/SUBSCRIPTION_WORKERS.md</code>).</p>
        <p v-if="row.provider === 'claude' && row.state === 'connected'">Claude login is linked. Verify extra usage is disabled or capped at zero in your provider account before explicitly enabling the worker socket.</p>
        <p>Runtime install, update and rollback: <button class="small ghost" @click="emit('open', 'releases')">Open Releases</button></p>
        <div class="dialog-actions">
          <button v-if="row.state !== 'connected'" class="small secondary" :disabled="busy" aria-describedby="subscriptions-hint" @click="account(row.provider, 'connect')">{{ label(`connect:${row.provider}`, "Connect") }}</button>
          <template v-else>
            <button class="small secondary" :disabled="busy" aria-describedby="subscriptions-hint" @click="account(row.provider, 'test')">{{ label(`test:${row.provider}`, "Test") }}</button>
            <button class="small secondary" :disabled="busy" @click="account(row.provider, 'reconnect')">{{ label(`reconnect:${row.provider}`, "Reconnect…") }}</button>
            <button class="small danger" :disabled="busy" @click="account(row.provider, 'disconnect')">{{ label(`disconnect:${row.provider}`, "Disconnect…") }}</button>
          </template>
        </div>
      </article>
    </div>
  </section>

  <section class="accounts-section" aria-labelledby="keys-title">
    <h2 id="keys-title">API keys</h2>
    <p id="keys-hint" class="section-hint">Set and Replace open a trusted terminal window and ask for system administrator authorization. You type the key there, hidden; it goes straight to protected storage and never enters this app, Tauri IPC, logs or process arguments. The key is checked before it replaces a stored one, and that provider's model list is refreshed; new models start disabled. “Configured” means stored, not that the provider accepts it right now.</p>
    <ErrorPanel v-if="errors.keys" title="Could not load API keys" :message="errors.keys" />
    <ErrorPanel v-if="errors.usage" title="Could not read which models use each key" :message="errors.usage" />
    <div class="credential-grid">
      <article v-for="row in keys" :key="row.provider" class="metric-card credential-card" :aria-labelledby="`key-${row.provider}`">
        <h3 :id="`key-${row.provider}`" class="card-label">{{ credentialLabels[row.provider] }}</h3>
        <StatusBadge :state="row.configured ? 'configured' : 'not configured'" :tone="needed(row) ? 'warn' : undefined" />
        <p>{{ keyUseLabel(useOf(row.provider)) }}<template v-if="needed(row)">; those calls fail without a key</template></p>
        <button v-if="row.configured" class="small secondary" :disabled="busy" @click="setKey(row)">{{ label(`key:${row.provider}`, "Replace…") }}</button>
        <button v-else class="small secondary" :disabled="busy" aria-describedby="keys-hint" @click="setKey(row)">{{ label(`key:${row.provider}`, "Set key") }}</button>
      </article>
    </div>
  </section>

  <ConfirmDialog v-if="confirm" v-bind="confirm.spec" @cancel="confirm = null" @confirm="confirm.run()" />
</template>

<style scoped>
.accounts-section { margin-top: 20px; }
.accounts-section h2 { margin: 0 0 4px; font-size: 15px; }
.section-hint { max-width: 720px; margin: 0 0 12px; color: var(--text-secondary); font-size: 13px; line-height: 1.5; }
.credential-card h3 { margin: 0; }
.credential-card h3::first-letter { text-transform: none; } /* provider names such as xAI */
</style>
