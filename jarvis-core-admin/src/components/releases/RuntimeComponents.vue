<script setup lang="ts">
import { computed, onMounted, ref } from "vue";
import { api, errorText, type ClaudeRuntimeMutation, type ClaudeRuntimeStatus, type CodexRuntimeMutation, type CodexRuntimeStatus, type OperationResult, type RuntimeChannel, strictRuntimeVersion } from "../../admin";
import ConfirmDialog from "../ConfirmDialog.vue";
import StatusBadge from "../StatusBadge.vue";
import { operations } from "./operations";
import { isRunning, runtimeBadge, track } from "./release";

// The Claude Code and Codex CLI runtimes the subscription workers run. Moved
// from AI accounts; the broker calls and dialogs are unchanged.
const props = defineProps<{ disabled?: boolean }>();
const runtime = ref<ClaudeRuntimeStatus | null>(null);
const runtimeError = ref("");
const channel = ref<RuntimeChannel>("stable");
const confirmRuntime = ref<ClaudeRuntimeMutation | null>(null);
const codexRuntime = ref<CodexRuntimeStatus | null>(null);
const codexRuntimeError = ref("");
const codexRelease = ref<"latest" | "version">("latest");
const codexVersion = ref("");
const confirmCodexRuntime = ref<CodexRuntimeMutation | null>(null);
const loading = ref(false);
const busy = computed(() => loading.value || !!props.disabled || isRunning(operations));

async function loadRuntime() {
  loading.value = true;
  runtimeError.value = "";
  codexRuntimeError.value = "";
  try { runtime.value = await api.claudeRuntime(); }
  catch (e) { runtime.value = null; runtimeError.value = errorText(e); }
  try { codexRuntime.value = await api.codexRuntime(); }
  catch (e) { codexRuntime.value = null; codexRuntimeError.value = errorText(e); }
  loading.value = false;
}

function codexInstallRequest(): CodexRuntimeMutation | null {
  if (codexRelease.value === "latest") return { action: "install_latest" };
  const version = codexVersion.value.trim();
  return strictRuntimeVersion(version) ? { action: "install_version", version } : null;
}

const NOTE = "The trusted installer owns this step and finishes it even if this session locks.";
async function runtimeAction<T>(mutate: (request: T) => Promise<OperationResult>, request: T, dialog: Dialog) {
  confirmRuntime.value = null;
  confirmCodexRuntime.value = null;
  await track(operations, { doing: dialog.doing, done: dialog.label, note: NOTE }, () => mutate(request), errorText);
  await loadRuntime();
}

type Dialog = { title: string; detail: string; label: string; doing: string; tone: "neutral" | "primary" };
function runtimeDialog(request: ClaudeRuntimeMutation): Dialog {
  if (request.action === "rollback") {
    return { title: "Roll back Claude Code runtime?", detail: "Restores the Claude Code binary that the last install replaced at /usr/local/bin/claude. That binary is not re-verified; its version is checked before any use.", label: "Roll back Claude Code", doing: "Rolling back Claude Code", tone: "neutral" };
  }
  return {
    title: `Install Claude Code (${request.channel})?`,
    detail: `Downloads the ${request.channel} Claude Code release for this platform from downloads.claude.ai over HTTPS, verifies the release manifest signature against the pinned Anthropic release key and the binary's SHA-256, refuses versions outside the reviewed 2.1.248+ (2.1) contract, and atomically replaces /usr/local/bin/claude. The current binary is kept for rollback.`,
    label: `Install Claude Code (${request.channel})`,
    doing: `Installing Claude Code (${request.channel})`,
    tone: "primary",
  };
}

function codexRuntimeDialog(request: CodexRuntimeMutation): Dialog {
  if (request.action === "rollback") {
    return { title: "Roll back Codex CLI runtime?", detail: "Restores the Codex binary that the last install replaced at /usr/local/bin/codex. That binary is not re-verified; the chat worker still checks its reviewed version before any use.", label: "Roll back Codex CLI", doing: "Rolling back Codex CLI", tone: "neutral" };
  }
  const release = request.action === "install_latest" ? "the latest release" : `release ${request.version}`;
  const version = request.action === "install_latest" ? "(latest)" : request.version;
  return {
    title: `Install Codex CLI (${request.action === "install_latest" ? "latest" : request.version})?`,
    detail: `Downloads ${release} of the official Codex CLI for this platform from github.com/openai/codex over HTTPS (one redirect, only to GitHub asset storage), checks the archive against GitHub's SHA-256 digest, extracts only the single codex binary, and verifies it with cosign against OpenAI's keyless Sigstore signature (exact rust-release.yml workflow identity at that tag, GitHub Actions issuer, Rekor entry). It then atomically replaces /usr/local/bin/codex; the current binary is kept for rollback. The chat worker still runs only the version you set as reviewed.`,
    label: `Install Codex CLI ${version}`,
    doing: `Installing Codex CLI ${version}`,
    tone: "primary",
  };
}

const claudeDialog = computed(() => confirmRuntime.value && runtimeDialog(confirmRuntime.value));
const codexDialog = computed(() => confirmCodexRuntime.value && codexRuntimeDialog(confirmCodexRuntime.value));
onMounted(loadRuntime);
</script>

<template>
  <section class="components" aria-labelledby="components-title">
    <div class="components-head">
      <h2 id="components-title">Components</h2>
      <button class="small secondary" :disabled="busy" @click="loadRuntime">Check runtimes</button>
    </div>
    <p class="components-hint">The official CLI runtimes the Claude Code and Codex subscription workers run. Sign-in stays under Accounts &amp; keys.</p>
    <div class="credential-grid">
      <article class="metric-card credential-card">
        <span class="card-label">Claude Code</span>
        <template v-if="runtime">
          <StatusBadge v-bind="runtimeBadge(runtime.installed, runtime.gate_ok && runtime.safe_ownership, runtime.update_available)" />
          <p>Official runtime: {{ runtime.installed ? (runtime.version ?? "installed, version unknown") : "not installed" }}<template v-if="runtime.installed"> · {{ runtime.gate_ok ? "reviewed version" : "outside reviewed version contract" }} · {{ runtime.safe_ownership ? "root-owned" : "unsafe ownership" }}</template><br>Latest stable: {{ runtime.latest_stable ?? "unavailable" }}<template v-if="runtime.update_available"> (update available)</template></p>
        </template>
        <p v-else-if="runtimeError">Runtime status unavailable: {{ runtimeError }}</p>
        <div class="dialog-actions">
          <select v-model="channel" :disabled="busy" aria-label="Claude Code release channel"><option value="stable">Stable</option><option value="latest">Latest</option></select>
          <button class="small secondary" :disabled="busy" @click="confirmRuntime = { action: 'install', channel }">Install / update runtime…</button>
          <button v-if="runtime?.rollback_available" class="small secondary" :disabled="busy" @click="confirmRuntime = { action: 'rollback' }">Roll back runtime…</button>
        </div>
      </article>
      <article class="metric-card credential-card">
        <span class="card-label">Codex CLI</span>
        <template v-if="codexRuntime">
          <StatusBadge v-bind="runtimeBadge(codexRuntime.installed, codexRuntime.safe_ownership, codexRuntime.update_available)" />
          <p>Official runtime: {{ codexRuntime.installed ? (codexRuntime.version ?? "installed, version unknown") : "not installed" }}<template v-if="codexRuntime.installed"> · {{ codexRuntime.safe_ownership ? "root-owned" : "unsafe ownership" }}</template><br>Latest release: {{ codexRuntime.latest ?? "unavailable" }}<template v-if="codexRuntime.update_available"> (update available)</template><br>cosign: {{ codexRuntime.cosign_available ? "installed" : "missing — run sudo apt install cosign" }}</p>
        </template>
        <p v-else-if="codexRuntimeError">Runtime status unavailable: {{ codexRuntimeError }}</p>
        <div class="dialog-actions">
          <select v-model="codexRelease" :disabled="busy" aria-label="Codex release"><option value="latest">Latest</option><option value="version">Version…</option></select>
          <input v-if="codexRelease === 'version'" v-model="codexVersion" :disabled="busy" aria-label="Codex version" placeholder="0.160.0" inputmode="numeric" maxlength="29" pattern="\d{1,9}\.\d{1,9}\.\d{1,9}">
          <button class="small secondary" :disabled="busy || !codexRuntime?.cosign_available || !codexInstallRequest()" @click="confirmCodexRuntime = codexInstallRequest()">Install / update runtime…</button>
          <button v-if="codexRuntime?.rollback_available" class="small secondary" :disabled="busy" @click="confirmCodexRuntime = { action: 'rollback' }">Roll back runtime…</button>
        </div>
      </article>
    </div>
  </section>
  <ConfirmDialog v-if="confirmRuntime && claudeDialog" :title="claudeDialog.title" :detail="claudeDialog.detail" :confirm-label="claudeDialog.label" :tone="claudeDialog.tone" @cancel="confirmRuntime = null" @confirm="runtimeAction(api.claudeRuntimeMutation, confirmRuntime, claudeDialog)" />
  <ConfirmDialog v-if="confirmCodexRuntime && codexDialog" :title="codexDialog.title" :detail="codexDialog.detail" :confirm-label="codexDialog.label" :tone="codexDialog.tone" @cancel="confirmCodexRuntime = null" @confirm="runtimeAction(api.codexRuntimeMutation, confirmCodexRuntime, codexDialog)" />
</template>

<style scoped>
.components { margin-top: 24px; }
.components-head { display: flex; align-items: center; justify-content: space-between; gap: 12px; }
.components-head h2 { margin: 0; font-size: 15px; }
.components-hint { margin: 4px 0 12px; color: var(--text-secondary); font-size: 13px; line-height: 1.5; }
</style>
