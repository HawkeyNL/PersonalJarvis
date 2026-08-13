<script setup lang="ts">
import { ref, onMounted } from "vue";
import { API_BASE, getJson, getJsonAuth, postJsonAuth } from "../api";
import { currentSession } from "../auth";

type Health = { status: string; environment?: string };
type Check = "checking" | "ok" | "fout";

const livez = ref<Check>("checking");
const readyz = ref<Check>("checking");
const environment = ref<string | null>(null);
const error = ref<string | null>(null);

function dotClass(c: Check): string {
  if (c === "ok") return "dot-ok";
  if (c === "fout") return "dot-err";
  return "dot-todo";
}

async function check() {
  livez.value = "checking";
  readyz.value = "checking";
  environment.value = null;
  error.value = null;

  try {
    await getJson<Health>("/livez");
    livez.value = "ok";
  } catch (e) {
    livez.value = "fout";
    error.value = String(e);
  }

  try {
    const r = await getJson<Health>("/readyz");
    readyz.value = "ok";
    environment.value = r.environment ?? null;
  } catch (e) {
    readyz.value = "fout";
    error.value = String(e);
  }
}

// --- AI-resource registry ("instant memory", ADR-027 stage 3) ---
interface Brain {
  id: string;
  label: string;
  cost: "plan" | "metered" | "local";
  available: boolean;
  note: string;
}
interface SoftwareItem {
  name: string;
  present: boolean;
  version: string | null;
  detail: string | null;
}
interface HostInfo {
  os: string;
  arch: string;
  cpu: string;
  cpu_cores: number;
  mem_total_gb: number;
  gpu: string;
}
interface Registry {
  host: HostInfo;
  software: SoftwareItem[];
  brains: Brain[];
  active_brain: string;
}

const reg = ref<Registry | null>(null);
const regError = ref<string | null>(null);
const regBusy = ref(false);
const costLabel: Record<Brain["cost"], string> = {
  plan: "plan",
  metered: "per-token",
  local: "lokaal",
};

async function loadRegistry(refresh = false) {
  regBusy.value = true;
  regError.value = null;
  try {
    const s = await currentSession();
    if (!s.token) {
      regError.value = "niet ingelogd";
      return;
    }
    reg.value = refresh
      ? await postJsonAuth<Registry>("/v1/system/registry/refresh", s.token, {})
      : await getJsonAuth<Registry>("/v1/system/registry", s.token);
  } catch (e) {
    regError.value = String(e);
  } finally {
    regBusy.value = false;
  }
}

onMounted(() => {
  check();
  loadRegistry();
});
</script>

<template>
  <section class="view">
    <h1>Status</h1>
    <p class="muted">
      Live-verbinding met de backend <code>jarvis-api</code> op
      <code>{{ API_BASE }}</code> (via de Tauri HTTP-plugin).
    </p>

    <ul class="status-list">
      <li>
        <span class="dot" :class="dotClass(livez)"></span>
        Liveness <code>/livez</code> — {{ livez }}
      </li>
      <li>
        <span class="dot" :class="dotClass(readyz)"></span>
        Readiness <code>/readyz</code> — {{ readyz }}
        <span v-if="environment">· {{ environment }}</span>
      </li>
    </ul>

    <button @click="check">Opnieuw controleren</button>
    <p v-if="error" class="muted err">Laatste fout: {{ error }}</p>

    <!-- AI-resources: brains Jarvis can route to, and the host it runs on. -->
    <div class="panel" v-if="reg || regError">
      <div class="phead">
        AI-RESOURCES <span class="hint">instant memory · router kiest per taak</span>
      </div>
      <p v-if="regError" class="muted err">{{ regError }}</p>
      <template v-else-if="reg">
        <p class="active">actief brein: <code>{{ reg.active_brain }}</code></p>
        <ul class="brains">
          <li v-for="b in reg.brains" :key="b.id">
            <span class="dot" :class="b.available ? 'dot-ok' : 'dot-err'"></span>
            <span class="blabel">{{ b.label }}</span>
            <span class="cost" :class="'cost-' + b.cost">{{ costLabel[b.cost] }}</span>
            <span class="muted small note">{{ b.note }}</span>
          </li>
        </ul>

        <div class="host">
          <span class="k">host</span>
          {{ reg.host.cpu }} · {{ reg.host.cpu_cores }} cores ·
          {{ reg.host.mem_total_gb }} GB · {{ reg.host.gpu }}
          <span class="muted">· {{ reg.host.os }} ({{ reg.host.arch }})</span>
        </div>

        <div class="sw">
          <span
            v-for="s in reg.software"
            :key="s.name"
            class="chip"
            :class="s.present ? 'on' : 'off'"
            :title="s.detail || ''"
          >
            {{ s.name }}<span v-if="s.version" class="ver"> {{ s.version }}</span>
          </span>
        </div>

        <button :disabled="regBusy" @click="loadRegistry(true)">
          {{ regBusy ? "verversen…" : "Ververs resources" }}
        </button>
      </template>
    </div>
  </section>
</template>

<style scoped>
.panel {
  margin-top: 22px;
  border: 1px solid var(--border);
  border-radius: 12px;
  padding: 14px 16px 16px;
  background: rgba(14, 30, 22, 0.5);
  backdrop-filter: blur(14px) saturate(1.3);
  -webkit-backdrop-filter: blur(14px) saturate(1.3);
  max-width: 640px;
}
.phead {
  display: flex;
  align-items: center;
  justify-content: space-between;
  font-family: var(--mono);
  font-size: 11px;
  letter-spacing: 0.18em;
  color: var(--accent);
  margin-bottom: 12px;
}
.hint {
  font-size: 9px;
  color: var(--muted);
  letter-spacing: 0.08em;
}
.active {
  margin: 0 0 12px;
  font-size: 13px;
}
.brains {
  list-style: none;
  margin: 0 0 12px;
  padding: 0;
  display: flex;
  flex-direction: column;
  gap: 9px;
}
.brains li {
  display: flex;
  align-items: center;
  gap: 9px;
  flex-wrap: wrap;
}
.blabel {
  font-size: 13px;
}
.cost {
  font-family: var(--mono);
  font-size: 9.5px;
  letter-spacing: 0.06em;
  padding: 2px 7px;
  border-radius: 999px;
  border: 1px solid var(--border);
}
.cost-plan {
  color: var(--accent);
  border-color: var(--accent);
}
.cost-metered {
  color: #fbbf24;
  border-color: rgba(251, 191, 36, 0.5);
}
.cost-local {
  color: #60a5fa;
  border-color: rgba(96, 165, 250, 0.5);
}
.note {
  margin-left: auto;
}
.host {
  font-size: 12px;
  color: var(--text);
  padding: 8px 0;
  border-top: 1px solid var(--border);
}
.host .k,
.sw .k {
  font-family: var(--mono);
  font-size: 9.5px;
  letter-spacing: 0.14em;
  color: var(--muted);
  margin-right: 8px;
}
.sw {
  display: flex;
  flex-wrap: wrap;
  gap: 7px;
  margin: 4px 0 14px;
}
.chip {
  font-size: 11px;
  padding: 3px 9px;
  border-radius: 999px;
  border: 1px solid var(--border);
  color: var(--muted);
}
.chip.on {
  color: var(--text);
  border-color: var(--accent-2, var(--accent));
}
.chip.off {
  opacity: 0.5;
  text-decoration: line-through;
}
.ver {
  opacity: 0.7;
}
.small {
  font-size: 12px;
}
.err {
  color: #f87171;
}
</style>
