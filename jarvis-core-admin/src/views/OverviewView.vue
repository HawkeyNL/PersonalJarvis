<script setup lang="ts">
import { computed, onMounted, ref } from "vue";
import {
  aiAccountLabels, api, credentialLabels, errorText,
  type AiAccountRecord, type CredentialRecord, type DeviceOverview, type ModelRecord,
  type OverviewResponse, type RouteTier, type RoutingReport, type Tone, type UsageReport, type ViewName,
} from "../admin";
import DailyUsageChart from "../components/DailyUsageChart.vue";
import JvModuleCard from "../components/jv/JvModuleCard.vue";
import JvOrb from "../components/jv/JvOrb.vue";
import JvStatusDot from "../components/jv/JvStatusDot.vue";
import { NOT_MEASURED, budgetPercent, compact, eur, integer, measured, ms, serviceSummary } from "../stats";

// Stats-first dashboard. Every number comes from the same trusted broker
// commands as the detail views; a source that fails only blanks its own card.
const emit = defineEmits<{ open: [view: ViewName] }>();
type Source<T> = { data: T | null; error: string };
const empty = <T,>(): Source<T> => ({ data: null, error: "" });
const overview = ref<Source<OverviewResponse>>(empty());
const usage = ref<Source<UsageReport>>(empty());
const models = ref<Source<ModelRecord[]>>(empty());
const routes = ref<Source<RoutingReport>>(empty());
const devices = ref<Source<DeviceOverview>>(empty());
const accounts = ref<Source<AiAccountRecord[]>>(empty());
const credentials = ref<Source<CredentialRecord[]>>(empty());
const busy = ref(false);
const updatedAt = ref("");

async function settle<T>(target: { value: Source<T> }, request: Promise<T>) {
  try { target.value = { data: await request, error: "" }; }
  catch (e) { target.value = { data: target.value.data, error: errorText(e) }; }
}
async function load() {
  busy.value = true;
  await Promise.all([
    settle(overview, api.overview()), settle(usage, api.usage()), settle(models, api.models()),
    settle(routes, api.modelRoutes()), settle(devices, api.devices()), settle(accounts, api.aiAccounts()),
    settle(credentials, api.credentials()),
  ]);
  updatedAt.value = new Date().toLocaleTimeString([], { hour: "2-digit", minute: "2-digit", hour12: false });
  busy.value = false;
}
onMounted(load);

const services = computed(() => serviceSummary(overview.value.data?.status.services));
const orbTone = computed<Tone>(() => (overview.value.error && !overview.value.data ? "error" : services.value.tone));
const headline = computed(() => {
  if (!overview.value.data) return overview.value.error ? "Home Node state unavailable" : "Reading Home Node state…";
  const { up, total } = services.value;
  if (!total) return "No Jarvis services reported";
  return up === total ? "All services running" : `${total - up} of ${total} services need attention`;
});
const unavailable = (error: string) => ({ value: "Unavailable", lines: [error], tone: "error" as Tone });
const pending = { value: "…", lines: ["Loading"], tone: "idle" as Tone };
// Core retries the usage aggregate while it starts; that is not a failure.
const usageMissing = () => usage.value.error.includes("usage statistics are unavailable")
  ? { value: "Initializing", lines: ["Core is preparing usage statistics"], tone: "idle" as Tone }
  : usage.value.error ? unavailable(usage.value.error) : pending;

const serviceCard = computed(() => {
  const data = overview.value.data;
  if (!data) return overview.value.error ? unavailable(overview.value.error) : pending;
  const down = Object.entries(data.status.services).filter(([, state]) => state.toLowerCase() !== "active").map(([name]) => name);
  return {
    value: `${services.value.up} / ${services.value.total} up`,
    lines: [down.length ? `Not active: ${down.join(", ")}` : "Every Jarvis service is active", `Updater ${data.status.updater_enabled}`],
    tone: services.value.tone,
  };
});
const releaseCard = computed(() => {
  const data = overview.value.data;
  if (!data) return overview.value.error ? unavailable(overview.value.error) : pending;
  const state = data.update?.update ?? "not checked";
  const lower = state.toLowerCase();
  return {
    value: data.status.release ?? "Unavailable",
    lines: [`Update: ${state}`, data.update?.latest ? `Latest release ${data.update.latest}` : "Check for updates in Update"],
    tone: (lower.includes("available") || lower.includes("required") ? "warn" : lower.includes("up to date") ? "ok" : "idle") as Tone,
  };
});
const agentCard = computed(() => {
  const data = overview.value.data;
  if (!data) return overview.value.error ? unavailable(overview.value.error) : pending;
  const bundle = data.status.agent_bundle;
  return bundle
    ? { value: `${bundle.agent_count} agents`, lines: [`Bundle ${bundle.id}`], tone: "ok" as Tone }
    : { value: "No bundle", lines: ["No active agent bundle reported"], tone: "warn" as Tone };
});
const spendCard = computed(() => {
  const data = usage.value.data;
  if (!data) return usageMissing();
  const percent = budgetPercent(data.spent_eur, data.budget_eur);
  return {
    value: `${eur(data.spent_eur)} / ${eur(data.budget_eur)}`,
    lines: [`${percent.toFixed(0)}% of the monthly budget · ${eur(data.remaining_eur)} remaining`, `${integer(data.requests)} requests · ${compact(data.total_tokens)} tokens`],
    tone: (data.over_budget ? "error" : data.above_soft_budget ? "warn" : "ok") as Tone,
    percent,
  };
});
const latencyCard = computed(() => {
  const data = usage.value.data;
  if (!data) return usageMissing();
  return {
    value: `p50 ${measured(data.latency_p50_ms, ms)}`,
    lines: [`p95 ${measured(data.latency_p95_ms, ms)}`, `Failures: ${measured(data.failures)}`, `Fallbacks: ${measured(data.fallbacks)}`],
    tone: ((data.failures ?? 0) > 0 ? "warn" : data.latency_p50_ms === null ? "idle" : "ok") as Tone,
  };
});
const TIERS: { tier: RouteTier; label: string }[] = [{ tier: "cheap", label: "Cheap" }, { tier: "default", label: "Default" }, { tier: "hard", label: "Hard" }];
const modelCard = computed(() => {
  const rows = models.value.data;
  if (!rows) return models.value.error ? unavailable(models.value.error) : pending;
  const enabled = rows.filter((row) => row.enabled).length;
  const report = routes.value.data;
  const paid = report?.routing_unavailable_reason ? "off (routing file needs repair)" : report ? report.routing?.paid_api ?? "allowed" : "unknown";
  return {
    value: `${enabled} enabled`,
    lines: [`of ${integer(rows.length)} discovered models`, `Paid APIs ${paid}`],
    tone: (!enabled || report?.routing_unavailable_reason ? "warn" : "ok") as Tone,
  };
});
const tiers = computed(() => TIERS.map(({ tier, label }) => {
  const route = routes.value.data?.routing?.tiers[tier];
  return { tier, label, detail: route ? `${route.chain.length} model${route.chain.length === 1 ? "" : "s"}` : "built-in", owner: !!route };
}));
const deviceCard = computed(() => {
  const data = devices.value.data;
  if (!data) return devices.value.error ? unavailable(devices.value.error) : pending;
  const requests = data.requests.length;
  return {
    value: `${data.devices.length} trusted`,
    lines: [requests ? `${requests} pending approval` : "No pending requests"],
    tone: (requests ? "warn" : "ok") as Tone,
  };
});
const accountCard = computed(() => {
  const rows = accounts.value.data;
  if (!rows) return accounts.value.error ? unavailable(accounts.value.error) : pending;
  const connected = rows.filter((row) => row.state === "connected").length;
  const troubled = rows.some((row) => !["connected", "logged_out"].includes(row.state));
  return {
    value: `${connected} / ${rows.length} connected`,
    lines: rows.map((row) => `${aiAccountLabels[row.provider]}: ${row.state.replace(/_/g, " ")}`),
    tone: (troubled ? "warn" : connected ? "ok" : "idle") as Tone,
  };
});
const credentialCard = computed(() => {
  const rows = credentials.value.data;
  if (!rows) return credentials.value.error ? unavailable(credentials.value.error) : pending;
  const configured = rows.filter((row) => row.configured).length;
  return { value: `${configured} / ${rows.length} configured`, lines: ["Storage status only; secret values never reach this app"], tone: (configured ? "ok" : "idle") as Tone };
});
</script>

<template>
  <div class="dash">
    <header class="dash-hero">
      <JvOrb class="dash-orb" :size="196" :tone="orbTone" still />
      <div class="hero-text">
        <p class="eyebrow">HOME NODE HEALTH</p>
        <h1>{{ headline }}</h1>
        <div class="hero-pill" role="status">
          <JvStatusDot :tone="orbTone" :label="orbTone === 'ok' ? 'Healthy' : orbTone === 'warn' ? 'Degraded' : orbTone === 'error' ? 'Attention' : 'Unknown'" />
          <span class="pill-sep" aria-hidden="true"></span>
          <span>{{ overview.data?.status.release ?? "Release unknown" }}</span>
        </div>
        <p class="hero-meta">
          <span v-if="busy" class="busy"><span class="spinner" /> Reading Home Node state</span>
          <span v-else-if="updatedAt">Updated {{ updatedAt }}</span>
          <button class="small" :disabled="busy" @click="load">Refresh</button>
        </p>
      </div>
    </header>

    <div class="dash-grid">
      <JvModuleCard icon="layers" title="SERVICES" v-bind="serviceCard" @open="emit('open', 'services')" />
      <JvModuleCard icon="download" title="RELEASE" v-bind="releaseCard" @open="emit('open', 'update')" />
      <JvModuleCard icon="agents" title="AGENTS" v-bind="agentCard" @open="emit('open', 'agents')" />

      <JvModuleCard class="wide" icon="chart" title="SPEND THIS MONTH" :value="spendCard.value" :lines="spendCard.lines" :tone="spendCard.tone" @open="emit('open', 'usage')">
        <template v-if="usage.data">
          <span class="meter" aria-hidden="true"><i :class="{ danger: usage.data.over_budget }" :style="{ width: `${budgetPercent(usage.data.spent_eur, usage.data.budget_eur)}%` }" /></span>
          <DailyUsageChart v-if="usage.data.daily.length" class="dash-chart" :rows="usage.data.daily" />
          <span v-else class="note">No recorded model calls this month.</span>
        </template>
      </JvModuleCard>
      <JvModuleCard icon="pulse" title="LATENCY &amp; RELIABILITY" v-bind="latencyCard" @open="emit('open', 'usage')">
        <span v-if="usage.data?.by_backend.length" class="backend-latency">
          <span v-for="row in usage.data.by_backend.slice(0, 4)" :key="row.backend">
            <span>{{ row.backend }}</span><span>{{ row.latency_p50_ms === null ? NOT_MEASURED : `${ms(row.latency_p50_ms)} · p95 ${measured(row.latency_p95_ms, ms)}` }}</span>
          </span>
        </span>
      </JvModuleCard>

      <JvModuleCard class="wide" icon="chip" title="MODELS &amp; ROUTING" v-bind="modelCard" @open="emit('open', 'models')">
        <span v-if="routes.data" class="chips">
          <span v-for="item in tiers" :key="item.tier" class="chip" :class="{ owner: item.owner }">{{ item.label }} · {{ item.detail }}</span>
        </span>
        <span v-else-if="routes.error" class="note">Routing unavailable: {{ routes.error }}</span>
      </JvModuleCard>
      <JvModuleCard icon="monitor" title="DEVICES" v-bind="deviceCard" @open="emit('open', 'devices')" />

      <JvModuleCard icon="link-2" title="AI ACCOUNTS" v-bind="accountCard" @open="emit('open', 'ai-accounts')" />
      <JvModuleCard class="wide" icon="key" title="CREDENTIALS" v-bind="credentialCard" @open="emit('open', 'credentials')">
        <span v-if="credentials.data" class="chips">
          <span v-for="row in credentials.data" :key="row.provider" class="chip" :class="{ owner: row.configured }">{{ credentialLabels[row.provider] }} · {{ row.configured ? "configured" : "not set" }}</span>
        </span>
      </JvModuleCard>
    </div>
  </div>
</template>

<style scoped>
.dash { max-width: 1180px; margin: 0 auto; }
.dash-hero { display: flex; align-items: center; justify-content: center; gap: 48px; margin: 4px 0 30px; }
.dash-orb { flex: none; }
.hero-text { min-width: 0; }
.hero-text h1 { margin: 10px 0 14px; font-family: var(--font-display); font-size: 30px; font-weight: 600; letter-spacing: 0.08em; color: var(--text-0); }
.hero-pill {
  display: inline-flex; align-items: center; gap: 14px; height: 40px; padding: 0 20px;
  border: 1px solid var(--line-a30); border-radius: 999px; background: rgba(4, 30, 24, 0.75); font-size: var(--fs-13); color: var(--text-2);
}
.pill-sep { width: 1px; height: 18px; background: var(--line-a30); }
.hero-meta { display: flex; align-items: center; gap: 14px; margin: 14px 0 0; color: var(--text-5); font-size: var(--fs-12); }
.dash-grid { display: grid; grid-template-columns: repeat(3, minmax(0, 1fr)); gap: 14px 16px; }
.dash-grid > .wide { grid-column: span 2; }
.meter { display: block; height: 6px; overflow: hidden; border-radius: 999px; background: rgba(var(--accent-rgb), 0.12); }
.meter i { display: block; height: 100%; min-width: 2px; border-radius: inherit; background: linear-gradient(90deg, var(--accent), var(--accent-2)); }
.meter i.danger { background: var(--danger); }
.dash-chart { height: 150px; margin-top: 10px; }
.chips { display: flex; flex-wrap: wrap; gap: 8px; }
.chip { padding: 4px 10px; border: 1px solid var(--line-a18); border-radius: 999px; color: var(--text-5); font-size: var(--fs-11); letter-spacing: 0.04em; }
.chip.owner { border-color: var(--line-a45); color: var(--text-2); }
.note { color: var(--text-5); font-size: var(--fs-12); }
.backend-latency { display: grid; gap: 6px; padding-top: 10px; border-top: 1px solid var(--line-a18); font-size: var(--fs-11); color: var(--text-4); }
.backend-latency > span { display: flex; justify-content: space-between; gap: 10px; }
.backend-latency > span > span:last-child { color: var(--text-2); font-variant-numeric: tabular-nums; white-space: nowrap; }
@media (max-width: 1099px), (max-height: 759px) {
  .dash-hero { gap: 24px; margin-bottom: 20px; }
  .dash-orb { display: none; }
  .hero-text h1 { font-size: 24px; }
  .dash-grid { grid-template-columns: repeat(2, minmax(0, 1fr)); }
}
</style>
