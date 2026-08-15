<script setup lang="ts">
import { ref, computed, onMounted } from "vue";
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
interface ModelEntry {
  id: string;
  backend: string;
  class: "light" | "mid" | "heavy" | "reasoning";
  cost: "local" | "cheap" | "mid" | "pricey";
  available: boolean;
}
interface Registry {
  host: HostInfo;
  software: SoftwareItem[];
  brains: Brain[];
  models: ModelEntry[];
  active_brain: string;
}

interface Usage {
  budget_eur: number;
  spent_eur: number;
  remaining_eur: number;
  over_budget: boolean;
  by_backend: { backend: string; spent_eur: number }[];
}

const reg = ref<Registry | null>(null);
const usage = ref<Usage | null>(null);
const regError = ref<string | null>(null);
const regBusy = ref(false);

function eur(n: number): string {
  return "€" + n.toFixed(2);
}
const budgetPct = computed(() => {
  const u = usage.value;
  if (!u || u.budget_eur <= 0) return 0;
  return Math.min(100, Math.round((u.spent_eur / u.budget_eur) * 100));
});
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
    usage.value = await getJsonAuth<Usage>("/v1/system/usage", s.token);
  } catch (e) {
    regError.value = String(e);
  } finally {
    regBusy.value = false;
  }
}

// Self-development (ADR-029 fase 4d): Jarvis proposes improvements to itself.
interface Proposal {
  title: string;
  category: string;
  rationale: string;
  cost: string;
  requires_approval: boolean;
  steps: string[];
}
interface SelfDev {
  summary: string;
  proposals: Proposal[];
  note: string;
}
const advice = ref<SelfDev | null>(null);
const adviceBusy = ref(false);
const adviceError = ref<string | null>(null);
const adviceCancelled = ref(false);
// Held while a request is in flight so the user can abort it — aborting also
// stops the server-side LLM call, so a cancel doesn't keep burning tokens.
const adviceCtrl = ref<AbortController | null>(null);

async function askSelfImprove() {
  adviceBusy.value = true;
  adviceError.value = null;
  adviceCancelled.value = false;
  const ctrl = new AbortController();
  adviceCtrl.value = ctrl;
  try {
    const s = await currentSession();
    if (!s.token) {
      adviceError.value = "niet ingelogd";
      return;
    }
    advice.value = await postJsonAuth<SelfDev>(
      "/v1/system/self-improve",
      s.token,
      {},
      ctrl.signal,
    );
  } catch (e) {
    // An abort is a deliberate cancel, not an error.
    if (ctrl.signal.aborted) {
      adviceCancelled.value = true;
    } else {
      adviceError.value = String(e);
    }
  } finally {
    adviceBusy.value = false;
    adviceCtrl.value = null;
  }
}

function cancelSelfImprove() {
  adviceCtrl.value?.abort();
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

        <!-- Monthly spend vs the hard budget (ADR-027). -->
        <div v-if="usage" class="budget">
          <div class="brow">
            <span class="k">maandbudget</span>
            <span :class="{ over: usage.over_budget }">
              {{ eur(usage.spent_eur) }} / {{ eur(usage.budget_eur) }}
              <span v-if="usage.over_budget" class="capped">· plafond bereikt</span>
            </span>
          </div>
          <div class="bar">
            <div
              class="fill"
              :class="{ over: usage.over_budget }"
              :style="{ width: budgetPct + '%' }"
            ></div>
          </div>
          <div v-if="usage.by_backend.length" class="bk">
            <span v-for="b in usage.by_backend" :key="b.backend" class="bkchip">
              {{ b.backend }} {{ eur(b.spent_eur) }}
            </span>
          </div>
        </div>
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

        <!-- Model catalog (ADR-028): what Jarvis can pick from, by class. -->
        <div v-if="reg.models && reg.models.length" class="models">
          <div class="k">modellen · goedkoopste geschikte per taak</div>
          <ul>
            <li
              v-for="m in reg.models"
              :key="m.backend + '/' + m.id"
              :class="{ off: !m.available }"
            >
              <span class="mclass" :class="'mc-' + m.class">{{ m.class }}</span>
              <span class="mid">{{ m.id }}</span>
              <span class="mcost">{{ m.cost }}</span>
            </li>
          </ul>
        </div>

        <button :disabled="regBusy" @click="loadRegistry(true)">
          {{ regBusy ? "verversen…" : "Ververs resources" }}
        </button>
      </template>
    </div>

    <!-- Self-development (ADR-029 4d): Jarvis proposes improvements to itself. -->
    <div class="panel">
      <div class="phead">
        ZELFVERBETERING <span class="hint">Jarvis stelt voor · jij keurt goed</span>
      </div>
      <p class="muted small">
        Jarvis bekijkt zijn eigen ecosysteem en doet voorstellen. Hij voert niets
        zelf uit — de Core en <code>Jarvis.md</code> blijven handmatig, alleen door jou.
      </p>
      <div class="sd-actions">
        <button v-if="!adviceBusy" @click="askSelfImprove">
          Vraag om verbetervoorstellen
        </button>
        <template v-else>
          <span class="thinking">
            Jarvis denkt na<span class="dots"><span></span><span></span><span></span></span>
          </span>
          <button class="ghost" @click="cancelSelfImprove">Annuleren</button>
        </template>
      </div>
      <p v-if="adviceCancelled" class="muted small">Geannuleerd.</p>
      <p v-if="adviceError" class="muted err">{{ adviceError }}</p>
      <template v-if="advice">
        <p class="active">{{ advice.summary }}</p>
        <ul class="props">
          <li v-for="(p, i) in advice.proposals" :key="i">
            <div class="prow">
              <span class="mclass mc-mid">{{ p.category }}</span>
              <span class="ptitle">{{ p.title }}</span>
              <span
                class="cost"
                :class="p.requires_approval ? 'cost-metered' : 'cost-local'"
              >
                {{ p.requires_approval ? "goedkeuring" : "vrij" }} · {{ p.cost }}
              </span>
            </div>
            <p v-if="p.rationale" class="muted small note">{{ p.rationale }}</p>
            <ol v-if="p.steps.length" class="steps">
              <li v-for="(st, j) in p.steps" :key="j">{{ st }}</li>
            </ol>
          </li>
        </ul>
        <p class="muted small note">{{ advice.note }}</p>
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
.budget {
  margin: 0 0 14px;
  font-size: 12px;
}
.brow {
  display: flex;
  justify-content: space-between;
  margin-bottom: 5px;
}
.brow .over {
  color: #f87171;
}
.capped {
  font-size: 10px;
  letter-spacing: 0.06em;
}
.bar {
  height: 6px;
  border-radius: 999px;
  background: rgba(255, 255, 255, 0.1);
  overflow: hidden;
}
.bar .fill {
  height: 100%;
  background: var(--accent);
  transition: width 0.3s ease;
}
.bar .fill.over {
  background: #f87171;
}
.bk {
  display: flex;
  flex-wrap: wrap;
  gap: 6px;
  margin-top: 8px;
}
.bkchip {
  font-family: var(--mono);
  font-size: 9.5px;
  letter-spacing: 0.04em;
  color: var(--muted);
  border: 1px solid var(--border);
  border-radius: 999px;
  padding: 2px 7px;
}
.models {
  border-top: 1px solid var(--border);
  padding-top: 10px;
  margin-bottom: 14px;
}
.models .k {
  font-family: var(--mono);
  font-size: 9.5px;
  letter-spacing: 0.12em;
  color: var(--muted);
  margin-bottom: 8px;
}
.models ul {
  list-style: none;
  margin: 0;
  padding: 0;
  display: flex;
  flex-direction: column;
  gap: 6px;
}
.models li {
  display: flex;
  align-items: center;
  gap: 8px;
  font-size: 12px;
}
.models li.off {
  opacity: 0.4;
}
.models li.off .mid {
  text-decoration: line-through;
}
.mclass {
  font-family: var(--mono);
  font-size: 9px;
  letter-spacing: 0.05em;
  padding: 1px 6px;
  border-radius: 999px;
  border: 1px solid var(--border);
  min-width: 62px;
  text-align: center;
}
.mc-light {
  color: #60a5fa;
  border-color: rgba(96, 165, 250, 0.5);
}
.mc-mid {
  color: var(--accent);
  border-color: var(--accent);
}
.mc-heavy {
  color: #fbbf24;
  border-color: rgba(251, 191, 36, 0.5);
}
.mc-reasoning {
  color: #c084fc;
  border-color: rgba(192, 132, 252, 0.5);
}
.mid {
  flex: 1;
  font-family: var(--mono);
  font-size: 11px;
}
.mcost {
  font-size: 9.5px;
  color: var(--muted);
  letter-spacing: 0.05em;
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
.props {
  list-style: none;
  padding: 0;
  margin: 12px 0 8px;
  display: flex;
  flex-direction: column;
  gap: 12px;
}
.props > li {
  border-left: 2px solid var(--border);
  padding-left: 10px;
}
.prow {
  display: flex;
  align-items: center;
  gap: 8px;
  flex-wrap: wrap;
}
.ptitle {
  font-size: 13px;
  font-weight: 600;
}
.steps {
  margin: 6px 0 0;
  padding-left: 18px;
  font-size: 12px;
  color: var(--muted);
}
.steps li {
  margin: 2px 0;
}

/* Self-development action row: a live "thinking" indicator + a cancel button. */
.sd-actions {
  display: flex;
  align-items: center;
  gap: 12px;
  flex-wrap: wrap;
}
.thinking {
  display: inline-flex;
  align-items: center;
  font-size: 13px;
  color: var(--accent);
}
.thinking .dots {
  display: inline-flex;
  gap: 4px;
  margin-left: 8px;
}
.thinking .dots span {
  width: 6px;
  height: 6px;
  border-radius: 50%;
  background: var(--accent);
  opacity: 0.4;
  animation: sdpulse 1.1s infinite ease-in-out;
}
.thinking .dots span:nth-child(2) {
  animation-delay: 0.18s;
}
.thinking .dots span:nth-child(3) {
  animation-delay: 0.36s;
}
@keyframes sdpulse {
  0%, 60%, 100% { opacity: 0.3; transform: translateY(0); }
  30% { opacity: 1; transform: translateY(-2px); }
}
button.ghost {
  background: transparent;
  border: 1px solid var(--border);
  color: var(--muted);
  font-weight: 500;
}
button.ghost:hover {
  filter: none;
  border-color: #f87171;
  color: #f87171;
}
@media (prefers-reduced-motion: reduce) {
  .thinking .dots span {
    animation: none;
  }
}
</style>
