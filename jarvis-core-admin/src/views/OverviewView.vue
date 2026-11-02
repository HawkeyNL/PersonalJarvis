<script setup lang="ts">
import { computed, onBeforeUnmount, onMounted, ref } from "vue";
import {
  aiAccountLabels, api, credentialLabels, errorText,
  type AiAccountRecord, type CredentialRecord, type DeviceOverview, type ModelRecord,
  type LogService, type OverviewResponse, type RouteTier, type RoutingReport, type UsageReport, type ViewName,
} from "../admin";
import { keyedState, stateLabel, toneOf, type Tone } from "../status";
import DailyUsageChart from "../components/DailyUsageChart.vue";
import JvModuleCard from "../components/jv/JvModuleCard.vue";
import PageHeader from "../components/PageHeader.vue";
import HealthSection from "../components/overview/HealthSection.vue";
import NeedsList from "../components/overview/NeedsList.vue";
import { needsYou, usageInitializing, type NeedItem, type Source } from "../components/overview/needs";
import { NOT_MEASURED, budgetPercent, compact, eur, integer, measured, ms } from "../stats";

// "Needs you" first, then health, then stats cards. Every number comes from the
// same trusted broker commands as the detail views; a source that fails only
// blanks its own section. The broker serializes requests anyway, so they run
// one after another in priority order and each section fills as its data lands.
const emit = defineEmits<{ open: [view: ViewName, service?: LogService]; pairings: [count: number] }>();
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
// A lock unmounts this view; requests still queued must not run after it.
let mounted = true;
onBeforeUnmount(() => { mounted = false; });

async function settle<T>(target: { value: Source<T> }, request: () => Promise<T>) {
  if (!mounted) return;
  try { target.value = { data: await request(), error: "" }; }
  catch (e) { target.value = { data: target.value.data, error: errorText(e) }; }
}
async function load() {
  if (busy.value) return;
  busy.value = true;
  await settle(overview, api.overview);
  await settle(devices, api.devices);
  if (devices.value.data) emit("pairings", devices.value.data.requests.length);
  await settle(routes, api.modelRoutes);
  await settle(usage, api.usage);
  await settle(credentials, api.credentials);
  await settle(models, api.models);
  await settle(accounts, api.aiAccounts);
  updatedAt.value = new Date().toLocaleTimeString([], { hour: "2-digit", minute: "2-digit", hour12: false });
  busy.value = false;
}
onMounted(load);

const needs = computed(() => needsYou({
  overview: overview.value, usage: usage.value, models: models.value, routes: routes.value, devices: devices.value, credentials: credentials.value,
}, credentialLabels));
function act(target: NeedItem["target"], service?: LogService) {
  if (target === "refresh") void load();
  else emit("open", target, service);
}
const description = computed(() => {
  const release = overview.value.data?.status.release;
  const checked = busy.value ? "Reading Home Node state…" : updatedAt.value ? `Checked ${updatedAt.value}` : "";
  return [release ? `Release ${release}` : "", checked].filter(Boolean).join(" · ");
});

const unavailable = (error: string) => ({ value: "Unavailable", lines: [error], tone: "error" as Tone });
const pending = { value: "…", lines: ["Loading"], tone: "idle" as Tone };
const usageMissing = () => usageInitializing(usage.value.error)
  ? { value: "Initializing", lines: ["Core is preparing usage statistics"], tone: "idle" as Tone }
  : usage.value.error ? unavailable(usage.value.error) : pending;

const releaseCard = computed(() => {
  const data = overview.value.data;
  if (!data) return overview.value.error ? unavailable(overview.value.error) : pending;
  const state = data.update?.update ? keyedState("update", data.update.update) : "";
  return {
    value: data.status.release ?? "Unavailable",
    lines: [`Update: ${state ? stateLabel(state) : "Not checked"}`, data.update?.latest ? `Latest release ${data.update.latest}` : "Check for updates in Releases"],
    tone: state ? toneOf(state) : "idle",
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
    lines: rows.map((row) => `${aiAccountLabels[row.provider]}: ${stateLabel(row.state)}`),
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
    <PageHeader title="Overview" :description="description" :busy="busy"><button class="secondary" @click="load">Refresh</button></PageHeader>

    <NeedsList :items="needs.items" :loading="needs.loading" :busy="busy" @act="act" />
    <HealthSection :services="overview.data?.status.services ?? null" :updater="overview.data?.status.updater_enabled ?? null" :error="overview.error" @logs="(service: LogService) => emit('open', 'logs', service)" />

    <h2 class="glance-title">At a glance</h2>
    <div class="dash-grid">
      <JvModuleCard icon="download" title="Releases" v-bind="releaseCard" @open="emit('open', 'releases')" />
      <JvModuleCard icon="agents" title="Agents" v-bind="agentCard" @open="emit('open', 'agents')" />
      <JvModuleCard icon="link-2" title="AI accounts" v-bind="accountCard" @open="emit('open', 'ai-accounts')" />

      <JvModuleCard class="wide" icon="chart" title="Spend this month" :value="spendCard.value" :lines="spendCard.lines" :tone="spendCard.tone" @open="emit('open', 'usage')">
        <template v-if="usage.data">
          <span class="meter" aria-hidden="true"><i :class="{ danger: usage.data.over_budget }" :style="{ width: `${budgetPercent(usage.data.spent_eur, usage.data.budget_eur)}%` }" /></span>
          <DailyUsageChart v-if="usage.data.daily.length" class="dash-chart" :rows="usage.data.daily" />
          <span v-else class="note">No recorded model calls this month.</span>
        </template>
      </JvModuleCard>
      <JvModuleCard icon="pulse" title="Latency &amp; reliability" v-bind="latencyCard" @open="emit('open', 'usage')">
        <span v-if="usage.data?.by_backend.length" class="backend-latency">
          <span v-for="row in usage.data.by_backend.slice(0, 4)" :key="row.backend">
            <span>{{ row.backend }}</span><span>{{ row.latency_p50_ms === null ? NOT_MEASURED : `${ms(row.latency_p50_ms)} · p95 ${measured(row.latency_p95_ms, ms)}` }}</span>
          </span>
        </span>
      </JvModuleCard>

      <JvModuleCard class="wide" icon="chip" title="Models &amp; routing" v-bind="modelCard" @open="emit('open', 'models')">
        <span v-if="routes.data" class="chips">
          <span v-for="item in tiers" :key="item.tier" class="chip" :class="{ owner: item.owner }">{{ item.label }} · {{ item.detail }}</span>
        </span>
        <span v-else-if="routes.error" class="note">Routing unavailable: {{ routes.error }}</span>
      </JvModuleCard>
      <JvModuleCard icon="monitor" title="Devices" v-bind="deviceCard" @open="emit('open', 'devices')" />

      <JvModuleCard class="full" icon="key" title="Credentials" v-bind="credentialCard" @open="emit('open', 'credentials')">
        <span v-if="credentials.data" class="chips">
          <span v-for="row in credentials.data" :key="row.provider" class="chip" :class="{ owner: row.configured }">{{ credentialLabels[row.provider] }} · {{ row.configured ? "configured" : "not set" }}</span>
        </span>
      </JvModuleCard>
    </div>
  </div>
</template>

<style scoped>
.dash { max-width: 1180px; margin: 0 auto; }
.glance-title { margin: 0 0 12px; font-size: 15px; }
.dash-grid { display: grid; grid-template-columns: repeat(3, minmax(0, 1fr)); gap: 16px; }
.dash-grid > .wide { grid-column: span 2; }
.dash-grid > .full { grid-column: 1 / -1; }
.dash-chart { height: 150px; margin-top: 10px; }
.chips { display: flex; flex-wrap: wrap; gap: 8px; }
.chip { display: inline-flex; align-items: center; height: 24px; padding: 0 9px; border-radius: var(--r-pill); background: var(--bg-sunken); color: var(--text-secondary); font-size: 12px; font-weight: 500; }
.chip.owner { background: var(--accent-subtle); color: var(--accent-text); }
.note { color: var(--text-secondary); font-size: 13px; }
.backend-latency { display: grid; gap: 6px; padding-top: 10px; border-top: 1px solid var(--separator); font-size: 13px; color: var(--text-secondary); }
.backend-latency > span { display: flex; justify-content: space-between; gap: 10px; }
.backend-latency > span > span:last-child { color: var(--text-primary); font-variant-numeric: tabular-nums; white-space: nowrap; }
@media (max-width: 1099px), (max-height: 759px) {
  .dash-grid { grid-template-columns: repeat(2, minmax(0, 1fr)); }
}
</style>
