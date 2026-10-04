<script setup lang="ts">
import { onMounted, ref } from "vue";
import { aiAccountLabels as labels, api, errorText, type AiAccountAction, type AiAccountProvider, type AiAccountRecord, type ClaudeRuntimeMutation, type ClaudeRuntimeStatus, type CodexRuntimeMutation, type CodexRuntimeStatus, type OperationResult, type RuntimeChannel, strictRuntimeVersion } from "../admin";
import ConfirmDialog from "../components/ConfirmDialog.vue";
import ErrorPanel from "../components/ErrorPanel.vue";
import PageHeader from "../components/PageHeader.vue";
import ResultPanel from "../components/ResultPanel.vue";
import StatusBadge from "../components/StatusBadge.vue";

const rows = ref<AiAccountRecord[]>([]);
const busy = ref(false);
const error = ref("");
const result = ref<OperationResult | null>(null);
const confirmDisconnect = ref<AiAccountProvider | null>(null);
const runtime = ref<ClaudeRuntimeStatus | null>(null);
const runtimeError = ref("");
const channel = ref<RuntimeChannel>("stable");
const confirmRuntime = ref<ClaudeRuntimeMutation | null>(null);
const codexRuntime = ref<CodexRuntimeStatus | null>(null);
const codexRuntimeError = ref("");
const codexRelease = ref<"latest" | "version">("latest");
const codexVersion = ref("");
const confirmCodexRuntime = ref<CodexRuntimeMutation | null>(null);

async function loadRuntime() {
  runtimeError.value = "";
  codexRuntimeError.value = "";
  try { runtime.value = await api.claudeRuntime(); }
  catch (e) { runtime.value = null; runtimeError.value = errorText(e); }
  try { codexRuntime.value = await api.codexRuntime(); }
  catch (e) { codexRuntime.value = null; codexRuntimeError.value = errorText(e); }
}

function codexInstallRequest(): CodexRuntimeMutation | null {
  if (codexRelease.value === "latest") return { action: "install_latest" };
  const version = codexVersion.value.trim();
  return strictRuntimeVersion(version) ? { action: "install_version", version } : null;
}

async function load() {
  busy.value = true;
  error.value = "";
  try { rows.value = await api.aiAccounts(); }
  catch (e) { error.value = errorText(e); }
  await loadRuntime();
  busy.value = false;
}

async function runtimeAction<T>(mutate: (request: T) => Promise<OperationResult>, request: T) {
  confirmRuntime.value = null;
  confirmCodexRuntime.value = null;
  busy.value = true;
  error.value = "";
  result.value = null;
  try {
    result.value = await mutate(request);
    rows.value = await api.aiAccounts();
  } catch (e) { error.value = errorText(e); }
  await loadRuntime();
  busy.value = false;
}

function runtimeDialog(request: ClaudeRuntimeMutation): { title: string; detail: string; label: string } {
  if (request.action === "rollback") {
    return { title: "Roll back Claude Code runtime?", detail: "Restores the Claude Code binary that the last install replaced at /usr/local/bin/claude. That binary is not re-verified; its version is checked before any use.", label: "Roll back" };
  }
  return {
    title: `Install Claude Code (${request.channel})?`,
    detail: `Downloads the ${request.channel} Claude Code release for this platform from downloads.claude.ai over HTTPS, verifies the release manifest signature against the pinned Anthropic release key and the binary's SHA-256, refuses versions outside the reviewed 2.1.248+ (2.1) contract, and atomically replaces /usr/local/bin/claude. The current binary is kept for rollback.`,
    label: "Install runtime",
  };
}

function codexRuntimeDialog(request: CodexRuntimeMutation): { title: string; detail: string; label: string } {
  if (request.action === "rollback") {
    return { title: "Roll back Codex CLI runtime?", detail: "Restores the Codex binary that the last install replaced at /usr/local/bin/codex. That binary is not re-verified; the chat worker still checks its reviewed version before any use.", label: "Roll back" };
  }
  const release = request.action === "install_latest" ? "the latest release" : `release ${request.version}`;
  return {
    title: `Install Codex CLI (${request.action === "install_latest" ? "latest" : request.version})?`,
    detail: `Downloads ${release} of the official Codex CLI for this platform from github.com/openai/codex over HTTPS (one redirect, only to GitHub asset storage), checks the archive against GitHub's SHA-256 digest, extracts only the single codex binary, and verifies it with cosign against OpenAI's keyless Sigstore signature (exact rust-release.yml workflow identity at that tag, GitHub Actions issuer, Rekor entry). It then atomically replaces /usr/local/bin/codex; the current binary is kept for rollback. The chat worker still runs only the version you set as reviewed.`,
    label: "Install runtime",
  };
}

async function act(provider: AiAccountProvider, action: AiAccountAction) {
  confirmDisconnect.value = null;
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
      <p v-if="row.state === 'incompatible_runtime'">The installed Claude CLI is outside the reviewed version contract. Recheck after an owner-reviewed CLI update before enabling the worker socket.</p>
      <p v-if="row.state === 'host_unsupported'">A host system tool the worker runs through (systemd-run or env) failed the root-ownership check. Connect reports which one.</p>
      <template v-if="row.provider === 'claude'">
        <p v-if="runtime">Official runtime: {{ runtime.installed ? (runtime.version ?? "installed, version unknown") : "not installed" }}<template v-if="runtime.installed"> · {{ runtime.gate_ok ? "reviewed version" : "outside reviewed version contract" }} · {{ runtime.safe_ownership ? "root-owned" : "unsafe ownership" }}</template><br>Latest stable: {{ runtime.latest_stable ?? "unavailable" }}<template v-if="runtime.update_available"> (update available)</template></p>
        <p v-else-if="runtimeError">Runtime status unavailable: {{ runtimeError }}</p>
        <div class="dialog-actions">
          <select v-model="channel" :disabled="busy" aria-label="Claude Code release channel"><option value="stable">Stable</option><option value="latest">Latest</option></select>
          <button class="small secondary" :disabled="busy" @click="confirmRuntime = { action: 'install', channel }">Install / update runtime…</button>
          <button v-if="runtime?.rollback_available" class="small secondary" :disabled="busy" @click="confirmRuntime = { action: 'rollback' }">Roll back runtime…</button>
        </div>
      </template>
      <template v-else-if="row.provider === 'codex'">
        <p v-if="codexRuntime">Official runtime: {{ codexRuntime.installed ? (codexRuntime.version ?? "installed, version unknown") : "not installed" }}<template v-if="codexRuntime.installed"> · {{ codexRuntime.safe_ownership ? "root-owned" : "unsafe ownership" }}</template><br>Latest release: {{ codexRuntime.latest ?? "unavailable" }}<template v-if="codexRuntime.update_available"> (update available)</template><br>cosign: {{ codexRuntime.cosign_available ? "installed" : "missing — run sudo apt install cosign" }}</p>
        <p v-else-if="codexRuntimeError">Runtime status unavailable: {{ codexRuntimeError }}</p>
        <div class="dialog-actions">
          <select v-model="codexRelease" :disabled="busy" aria-label="Codex release"><option value="latest">Latest</option><option value="version">Version…</option></select>
          <input v-if="codexRelease === 'version'" v-model="codexVersion" :disabled="busy" aria-label="Codex version" placeholder="0.160.0" inputmode="numeric" maxlength="29" pattern="\d{1,9}\.\d{1,9}\.\d{1,9}">
          <button class="small secondary" :disabled="busy || !codexRuntime?.cosign_available || !codexInstallRequest()" @click="confirmCodexRuntime = codexInstallRequest()">Install / update runtime…</button>
          <button v-if="codexRuntime?.rollback_available" class="small secondary" :disabled="busy" @click="confirmCodexRuntime = { action: 'rollback' }">Roll back runtime…</button>
        </div>
      </template>
      <p v-else>Official runtime: the shared <code>/usr/local/bin/codex</code>, installed from the Codex (coding) card.</p>
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
  <ConfirmDialog v-if="confirmRuntime" :title="runtimeDialog(confirmRuntime).title" :detail="runtimeDialog(confirmRuntime).detail" :confirm-label="runtimeDialog(confirmRuntime).label" @cancel="confirmRuntime = null" @confirm="runtimeAction(api.claudeRuntimeMutation, confirmRuntime)" />
  <ConfirmDialog v-if="confirmCodexRuntime" :title="codexRuntimeDialog(confirmCodexRuntime).title" :detail="codexRuntimeDialog(confirmCodexRuntime).detail" :confirm-label="codexRuntimeDialog(confirmCodexRuntime).label" @cancel="confirmCodexRuntime = null" @confirm="runtimeAction(api.codexRuntimeMutation, confirmCodexRuntime)" />
  <ConfirmDialog v-if="confirmDisconnect" :title="`Disconnect ${labels[confirmDisconnect]}?`" detail="Active runs may fail. Reconnecting opens a trusted terminal and requires system administrator authorization again." confirm-label="Disconnect" @cancel="confirmDisconnect = null" @confirm="act(confirmDisconnect, 'disconnect')" />
</template>
