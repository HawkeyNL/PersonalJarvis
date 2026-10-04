<script setup lang="ts">
import { computed, onMounted, ref } from "vue";
import { api, errorText, type LayaMode, type LayaMutation, type LayaStatus, type OperationResult } from "../admin";
import ConfirmDialog from "./ConfirmDialog.vue";
import ErrorPanel from "./ErrorPanel.vue";
import ResultPanel from "./ResultPanel.vue";
import StatusBadge from "./StatusBadge.vue";
import JvSegmented from "./jv/JvSegmented.vue";

const status = ref<LayaStatus | null>(null);
const busy = ref(false);
const error = ref("");
const result = ref<OperationResult | null>(null);
const pending = ref<LayaMutation | null>(null);
const modes = [{ id: "off", label: "Off" }, { id: "shadow", label: "Shadow" }, { id: "primary", label: "Primary" }];
const explanations: Record<LayaMode, string> = {
  off: "Laya is not consulted. Jev (remote) classifies when configured, otherwise normal Auto routing applies.",
  shadow: "Jev still decides the route. Laya classifies the same turn locally for comparison only and never changes the route. Recommended until a local benchmark justifies primary.",
  primary: "A Laya label at or above its confidence threshold (default 0.95) picks the advisory work kind; below it Jev is tried once, then normal Auto routing. Labels stay advice: policy, the model allowlist and approvals are unchanged.",
};
const enabled = computed(() => status.value?.socket_enabled === "enabled");
const gb = (bytes: number) => `${(bytes / 1e9).toFixed(2)} GB`;

async function load() {
  error.value = "";
  try { status.value = await api.laya(); }
  catch (e) { status.value = null; error.value = errorText(e); }
}

async function run(request: LayaMutation) {
  pending.value = null;
  busy.value = true;
  error.value = "";
  result.value = null;
  try { result.value = await api.layaMutation(request); }
  catch (e) { error.value = errorText(e); }
  await load();
  busy.value = false;
}

function chooseMode(id: string) {
  if (busy.value || !status.value || id === status.value.mode) return;
  pending.value = { action: "mode", mode: id as LayaMode };
}

function dialog(request: LayaMutation): { title: string; detail: string; label: string } {
  switch (request.action) {
    case "install":
      return {
        title: "Install Laya locally?",
        detail: `Downloads ${gb(status.value?.download_bytes ?? 0)} over HTTPS: 44 hash-pinned Python wheels (CPU-only PyTorch from download.pytorch.org, the rest from PyPI) and the English and multilingual Laya checkpoints from Hugging Face at the pinned revision ${status.value?.model_revision.slice(0, 12) ?? ""}. Every file's SHA-256 (and each wheel's exact size) is checked against the pins compiled into the jarvis CLI before it is staged; nothing from the model repository is executed. provision-laya then installs offline as the unprivileged jarvis-laya user without network access. Needs about 5 GB of disk and CPython 3.14 with venv support. Afterwards the local socket is enabled and Core restarts in shadow mode.`,
        label: "Download and install",
      };
    case "enable":
      return { title: "Enable Laya?", detail: "Enables jarvis-laya.socket, starts the service and probes its health (loading both checkpoints can take a minute). If Core's mode is off it becomes shadow and Core restarts. A failed probe turns Laya off again.", label: "Enable" };
    case "disable":
      return { title: "Turn Laya off?", detail: "Core restarts with Laya mode off, then jarvis-laya.socket and jarvis-laya.service are stopped and disabled. Installed files stay on disk; Enable turns it back on without downloading.", label: "Turn off" };
    case "mode":
      return { title: `Switch Laya to ${request.mode}?`, detail: `${explanations[request.mode]} Core restarts and must report ready; otherwise the previous mode is restored.`, label: `Use ${request.mode}` };
  }
}

onMounted(load);
</script>

<template>
  <article class="metric-card laya-card">
    <span class="card-label">LOCAL SYSTEM-1 CLASSIFIER · LAYA</span>
    <ErrorPanel v-if="error" :message="error" />
    <ResultPanel v-if="result" :result="result" />
    <template v-if="status">
      <p>
        Installed: {{ status.installed ? `yes (laya ${status.laya_version}, model ${status.model_revision.slice(0, 12)})` : "no" }}<br>
        Socket: <StatusBadge :state="status.socket_enabled" /> · <StatusBadge :state="status.socket_active" /> &nbsp; Service: <StatusBadge :state="status.service_active" /><br>
        Last probe: {{ status.last_probe ? `${status.last_probe.ok ? "healthy" : "failed"} at ${new Date(status.last_probe.at * 1000).toLocaleString()}` : "never" }} · Disk used: {{ gb(status.disk_bytes) }}
      </p>
      <template v-if="enabled && status.mode !== 'unreadable'">
        <JvSegmented :items="modes" :model-value="status.mode" label="Laya routing mode" @update:model-value="chooseMode" />
        <p class="usage-source">{{ explanations[status.mode] }}</p>
      </template>
      <p v-else class="usage-source">Core mode: {{ status.mode }}. {{ status.installed ? "Enable Laya to choose Shadow or Primary." : "Laya is optional; Jev or normal Auto routing is used without it." }}</p>
    </template>
    <div class="dialog-actions">
      <button class="small secondary" :disabled="busy" @click="load">Check status</button>
      <button v-if="status && !status.installed" class="small" :disabled="busy" @click="pending = { action: 'install' }">Install…</button>
      <button v-else-if="status && !enabled" class="small secondary" :disabled="busy" @click="pending = { action: 'enable' }">Enable…</button>
      <button v-else-if="status" class="small danger" :disabled="busy" @click="pending = { action: 'disable' }">Turn off…</button>
    </div>
  </article>
  <ConfirmDialog v-if="pending" :title="dialog(pending).title" :detail="dialog(pending).detail" :confirm-label="dialog(pending).label" @cancel="pending = null" @confirm="run(pending)" />
</template>
