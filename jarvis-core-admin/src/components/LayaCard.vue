<script setup lang="ts">
import { computed, onMounted, ref } from "vue";
import { api, errorText, type LayaMode, type LayaMutation, type LayaStatus, type OperationResult } from "../admin";
import ConfirmDialog from "./ConfirmDialog.vue";
import ErrorPanel from "./ErrorPanel.vue";
import ResultPanel from "./ResultPanel.vue";
import StatusBadge from "./StatusBadge.vue";
import { stateLabel } from "../status";
import JvSegmented from "./jv/JvSegmented.vue";

const status = ref<LayaStatus | null>(null);
const busy = ref(false);
const error = ref("");
const errorTitle = ref<string>();
const result = ref<OperationResult | null>(null);
const pending = ref<LayaMutation | null>(null);
const modes = [{ id: "off", label: "Off" }, { id: "shadow", label: "Shadow" }, { id: "primary", label: "Primary" }];
const explanations: Record<LayaMode, string> = {
  off: "Laya is not consulted. Jev (remote) classifies when configured, otherwise normal Auto routing applies.",
  shadow: "Jev still decides the route. Laya classifies the same turn locally for comparison only and never changes the route. Recommended until a local benchmark justifies primary.",
  primary: "A Laya label at or above its confidence threshold (default 0.95) picks the advisory work kind; below it Jev is tried once, then normal Auto routing. Labels stay advice: policy, the model allowlist and approvals are unchanged.",
};
const enabled = computed(() => status.value?.socket_enabled === "enabled");
// A stopped unit is expected while Laya is turned off; a failed one is not.
const unitTone = (state: string) => (!enabled.value && state === "inactive" ? "idle" : undefined);
const gb = (bytes: number) => `${(bytes / 1e9).toFixed(2)} GB`;

async function load() {
  error.value = "";
  try { status.value = await api.laya(); }
  catch (e) { status.value = null; error.value = errorText(e); errorTitle.value = "Could not load Laya status"; }
}

async function run(request: LayaMutation) {
  pending.value = null;
  busy.value = true;
  error.value = "";
  result.value = null;
  try { result.value = await api.layaMutation(request); }
  catch (e) { error.value = errorText(e); errorTitle.value = undefined; }
  await load();
  busy.value = false;
}

function chooseMode(id: string) {
  if (busy.value || !status.value || id === status.value.mode) return;
  pending.value = { action: "mode", mode: id as LayaMode };
}

function dialog(request: LayaMutation): { title: string; detail: string; label: string; tone: "neutral" | "primary" | "danger" } {
  switch (request.action) {
    case "install":
      return {
        title: "Install Laya locally?",
        detail: `Downloads ${gb(status.value?.download_bytes ?? 0)} over HTTPS: 44 hash-pinned Python wheels (CPU-only PyTorch from download.pytorch.org, the rest from PyPI) and the English and multilingual Laya checkpoints from Hugging Face at the pinned revision ${status.value?.model_revision.slice(0, 12) ?? ""}. Every file's SHA-256 (and each wheel's exact size) is checked against the pins compiled into the jarvis CLI before it is staged; nothing from the model repository is executed. provision-laya then installs offline as the unprivileged jarvis-laya user without network access. Keeps about 4.5 GB on disk and needs 8 GiB free, plus CPython 3.14 with venv support. Afterwards the Laya socket and service are enabled (started at boot, models kept in memory) and Core restarts in shadow mode.`,
        label: "Download and install Laya",
        tone: "primary",
      };
    case "enable":
      return { title: "Enable Laya?", detail: "Enables and starts jarvis-laya.socket and jarvis-laya.service (also at boot, keeping both checkpoints in memory) and probes its health; loading can take a minute. If Core's mode is off it becomes shadow and Core restarts. A failed probe turns Laya off again.", label: "Enable Laya", tone: "primary" };
    case "disable":
      return { title: "Turn Laya off?", detail: "Core restarts with Laya mode off, then jarvis-laya.socket and jarvis-laya.service are always stopped and disabled, even if Core is slow to report ready. Installed files stay on disk; Enable turns it back on without downloading.", label: "Turn Laya off", tone: "danger" };
    case "mode":
      return { title: `Switch Laya to ${request.mode}?`, detail: `${explanations[request.mode]} ${request.mode === "off" ? "Core restarts; off stays saved even if Core is slow to report ready." : "Core restarts and must report ready; otherwise the previous mode is restored."}`, label: `Use ${request.mode} mode`, tone: "neutral" };
  }
}

onMounted(load);
</script>

<template>
  <article class="metric-card laya-card">
    <span class="card-label">Local classifier (Laya)</span>
    <ErrorPanel v-if="error" :message="error" :title="errorTitle" />
    <ResultPanel v-if="result" :result="result" />
    <template v-if="status">
      <dl class="laya-facts">
        <dt>Installed</dt><dd>{{ status.installed ? `Laya ${status.laya_version}, model ${status.model_revision.slice(0, 12)}` : "Not installed" }}</dd>
        <dt>Starts at boot</dt><dd><StatusBadge :state="status.socket_enabled" /></dd>
        <dt>Socket</dt><dd><StatusBadge :state="status.socket_active" :tone="unitTone(status.socket_active)" /></dd>
        <dt>Service</dt><dd><StatusBadge :state="status.service_active" :tone="unitTone(status.service_active)" /></dd>
        <dt>Last health check</dt><dd>{{ status.last_probe ? `${status.last_probe.ok ? "Healthy" : "Failed"} at ${new Date(status.last_probe.at * 1000).toLocaleString()}` : "Never" }}</dd>
        <dt>Disk used</dt><dd>{{ gb(status.disk_bytes) }}</dd>
      </dl>
      <template v-if="enabled && status.mode !== 'unreadable'">
        <JvSegmented :items="modes" :model-value="status.mode" label="Laya routing mode" @update:model-value="chooseMode" />
        <p class="usage-source">{{ explanations[status.mode] }}</p>
      </template>
      <p v-else class="usage-source">Core mode: {{ stateLabel(status.mode) }}. {{ status.installed ? "Enable Laya to choose Shadow or Primary." : "Laya is optional; Jev or normal Auto routing is used without it." }}</p>
    </template>
    <div class="dialog-actions">
      <button class="small secondary" :disabled="busy" @click="load">Check status</button>
      <button v-if="status && !status.installed" class="small" :disabled="busy" @click="pending = { action: 'install' }">Install…</button>
      <button v-else-if="status && !enabled" class="small secondary" :disabled="busy" @click="pending = { action: 'enable' }">Enable…</button>
      <button v-else-if="status" class="small danger" :disabled="busy" @click="pending = { action: 'disable' }">Turn off…</button>
    </div>
  </article>
  <ConfirmDialog v-if="pending" :title="dialog(pending).title" :detail="dialog(pending).detail" :confirm-label="dialog(pending).label" :tone="dialog(pending).tone" @cancel="pending = null" @confirm="run(pending)" />
</template>

<style scoped>
.laya-facts { display: grid; grid-template-columns: max-content 1fr; align-items: center; gap: 6px 16px; margin: 0; font-size: 13px; }
</style>
