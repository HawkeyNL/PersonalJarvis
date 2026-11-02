<script setup lang="ts">
import { computed, onMounted, ref } from "vue";
import { api, errorText, type UsageReport } from "../admin";
import DailyUsageChart from "../components/DailyUsageChart.vue";
import ErrorPanel from "../components/ErrorPanel.vue";
import JvSegmented from "../components/jv/JvSegmented.vue";
import JvUnavailable from "../components/jv/JvUnavailable.vue";
import PageHeader from "../components/PageHeader.vue";
import { budgetPercent as percentOf, eur, integer, measured, ms } from "../stats";

const data = ref<UsageReport | null>(null);
const busy = ref(false);
const error = ref("");
const breakdown = ref("backend");
const BREAKDOWNS = [
  { id: "backend", label: "By backend" }, { id: "model", label: "By model" },
  { id: "agent", label: "By agent" }, { id: "failures", label: "Failures" },
];
const initializing = computed(() => !data.value && error.value.includes("usage statistics are unavailable"));
const budgetPercent = computed(() => (data.value ? percentOf(data.value.spent_eur, data.value.budget_eur) : 0));
const breakdownLabel = computed(() => BREAKDOWNS.find((item) => item.id === breakdown.value)!.label);
// Share of this month's tokens (or failed calls), shown as a meter plus its number.
const tokenShare = (tokens: number) => Math.round(percentOf(tokens, data.value?.total_tokens ?? 0));
const failureTotal = computed(() => data.value?.failures_by_category?.reduce((sum, row) => sum + row.requests, 0) ?? 0);

// Optional analysis cells: null reads "Not measured yet" and is styled apart.
const cell = (value: number | null | undefined, format: (value: number) => string = integer) =>
  ({ text: measured(value, format), unmeasured: value === null || value === undefined });
function lastUsed(value: string | null): string {
  if (!value) return "—";
  const date = new Date(value);
  return Number.isNaN(date.valueOf()) ? "—" : new Intl.DateTimeFormat(undefined, { dateStyle: "medium", timeStyle: "short" }).format(date);
}
async function load() {
  busy.value = true;
  error.value = "";
  try { data.value = await api.usage(); }
  catch (reason) { error.value = errorText(reason); }
  finally { busy.value = false; }
}
onMounted(load);
</script>

<template>
  <PageHeader title="Usage &amp; costs" description="Bounded monthly token, cost, latency and reliability aggregates. Prompts, replies, credentials and request identifiers never enter this view." :busy="busy">
    <button class="secondary" @click="load">Refresh</button>
  </PageHeader>
  <ErrorPanel v-if="error && !initializing" title="Could not load usage" :message="error" />
  <section v-if="initializing" class="detail-card empty-state">
    Usage statistics are initializing. Core retries the safe aggregate automatically; refresh this view in a moment.
  </section>
  <div v-else-if="!data && !error" class="empty-state card" role="status"><span><span class="spinner" aria-hidden="true"></span> Loading usage…</span></div>
  <template v-if="data">
    <div class="usage-top">
      <section class="card budget-card" aria-labelledby="budget-title">
        <h2 id="budget-title" class="card-label">Monthly hard budget</h2>
        <p class="budget-figure"><span class="big">{{ eur(data.spent_eur) }}</span><span class="t-2">of {{ eur(data.budget_eur) }} hard cap</span><strong :class="{ 'usage-danger': data.over_budget }">{{ budgetPercent.toFixed(0) }}%</strong></p>
        <div class="meter budget-meter"><i :class="{ danger: data.over_budget }" :style="{ width: `${budgetPercent}%` }" /></div>
        <dl class="budget-lines t-sm">
          <dt>Remaining</dt><dd class="mono">{{ eur(data.remaining_eur) }}</dd>
          <dt>Reserved</dt><dd class="mono">{{ eur(data.reserved_eur) }}</dd>
          <dt>Hard capacity left</dt><dd class="mono">{{ eur(data.remaining_hard_eur) }}</dd>
        </dl>
      </section>
      <section class="card chart-card" aria-labelledby="daily-title">
        <div class="card-heading"><h2 id="daily-title">Tokens by day</h2><span class="t-xs t-2">this month</span></div>
        <DailyUsageChart v-if="data.daily.length" :rows="data.daily" />
        <div v-else class="empty-state">No recorded model calls this month.</div>
      </section>
    </div>

    <section class="metric-grid usage-metrics" aria-label="This month">
      <article class="metric-card"><span class="card-label">Total tokens</span><strong>{{ integer(data.total_tokens) }}</strong><small>{{ integer(data.requests) }} model calls</small></article>
      <article class="metric-card"><span class="card-label">Input</span><strong>{{ integer(data.input_tokens) }}</strong><small>{{ integer(data.cache_read_tokens) }} cached</small></article>
      <article class="metric-card"><span class="card-label">Output</span><strong>{{ integer(data.output_tokens) }}</strong><small>{{ integer(data.cache_write_tokens) }} cache writes</small></article>
      <article class="metric-card"><span class="card-label">Latency p50</span><strong :class="{ unmeasured: data.latency_p50_ms === null }">{{ measured(data.latency_p50_ms, ms) }}</strong><small>Calls with a measured latency</small></article>
      <article class="metric-card"><span class="card-label">Latency p95</span><strong :class="{ unmeasured: data.latency_p95_ms === null }">{{ measured(data.latency_p95_ms, ms) }}</strong><small>Slowest 5% start above this</small></article>
      <article class="metric-card"><span class="card-label">Failures</span><strong :class="{ unmeasured: data.failures === null }">{{ measured(data.failures) }}</strong><small>Failed model calls this month</small></article>
      <article class="metric-card"><span class="card-label">Fallbacks</span><strong :class="{ unmeasured: data.fallbacks === null }">{{ measured(data.fallbacks) }}</strong><small>Router moves to a next model</small></article>
    </section>

    <section class="card tight usage-breakdown" aria-labelledby="breakdown-title">
      <div class="card-title">
        <h2 id="breakdown-title">Who used it</h2>
        <JvSegmented v-model="breakdown" class="breakdown-tabs" :items="BREAKDOWNS" label="Usage breakdown" controls="usage-breakdown" />
      </div>
      <div id="usage-breakdown" role="tabpanel" :aria-label="breakdownLabel" class="table-scroll">
        <table v-if="breakdown === 'backend'" class="tbl">
          <thead><tr><th>Backend</th><th class="num">Calls</th><th class="num">Tokens</th><th class="num">Spend</th><th class="num">Failures</th><th class="num">Fallbacks</th><th class="num">p50</th><th class="num">p95</th><th class="share">Token share</th></tr></thead>
          <tbody><tr v-for="row in data.by_backend" :key="row.backend">
            <td>{{ row.backend }}</td><td class="num">{{ integer(row.requests) }}</td><td class="num">{{ integer(row.total_tokens) }}</td><td class="num">{{ eur(row.spent_eur) }}</td>
            <td v-for="(item, index) in [cell(row.failures), cell(row.fallbacks), cell(row.latency_p50_ms, ms), cell(row.latency_p95_ms, ms)]" :key="index" class="num" :class="{ unmeasured: item.unmeasured }">{{ item.text }}</td>
            <td class="share"><span class="share-cell"><span class="meter"><i :style="{ width: `${tokenShare(row.total_tokens)}%` }" /></span><span class="num">{{ tokenShare(row.total_tokens) }}%</span></span></td>
          </tr></tbody>
        </table>
        <table v-else-if="breakdown === 'model'" class="tbl">
          <thead><tr><th>Provider</th><th>Model</th><th class="num">Calls</th><th class="num">Tokens</th><th class="num">Spend</th><th class="num">Failures</th><th class="num">Fallbacks</th><th class="share">Token share</th></tr></thead>
          <tbody><tr v-for="row in data.by_model" :key="`${row.backend}/${row.model}`">
            <td>{{ row.backend }}</td><td class="mono wrap-anywhere">{{ row.model }}</td><td class="num">{{ integer(row.requests) }}</td><td class="num">{{ integer(row.total_tokens) }}</td><td class="num">{{ eur(row.spent_eur) }}</td>
            <td v-for="(item, index) in [cell(row.failures), cell(row.fallbacks)]" :key="index" class="num" :class="{ unmeasured: item.unmeasured }">{{ item.text }}</td>
            <td class="share"><span class="share-cell"><span class="meter"><i :style="{ width: `${tokenShare(row.total_tokens)}%` }" /></span><span class="num">{{ tokenShare(row.total_tokens) }}%</span></span></td>
          </tr></tbody>
        </table>
        <template v-else-if="breakdown === 'agent'">
          <JvUnavailable v-if="data.by_agent === null" class="breakdown-unavailable" icon="agents" title="Usage per agent" detail="This Core does not record which agent made a model call yet, or its release predates per-agent usage. Nothing is shown as zero." />
          <table v-else class="tbl">
            <thead><tr><th>Agent</th><th class="num">Calls</th><th class="num">Tokens</th><th class="num">Spend</th><th class="num">Failures</th><th class="num">Fallbacks</th><th class="num">p50</th><th class="num">p95</th><th>Last used</th></tr></thead>
            <tbody><tr v-for="row in data.by_agent" :key="row.agent_id">
              <td class="mono">{{ row.agent_id }}</td><td class="num">{{ integer(row.requests) }}</td><td class="num">{{ integer(row.total_tokens) }}</td><td class="num">{{ eur(row.spent_eur) }}</td>
              <td v-for="(item, index) in [cell(row.failures), cell(row.fallbacks), cell(row.latency_p50_ms, ms), cell(row.latency_p95_ms, ms)]" :key="index" class="num" :class="{ unmeasured: item.unmeasured }">{{ item.text }}</td>
              <td>{{ lastUsed(row.last_used) }}</td>
            </tr></tbody>
          </table>
          <div v-if="data.by_agent?.length === 0" class="empty-state">No agent model calls recorded this month.</div>
        </template>
        <template v-else>
          <JvUnavailable v-if="data.failures_by_category === null" class="breakdown-unavailable" icon="alert" title="Failures by category" detail="This Core does not record failed model calls yet, or its release predates failure categories. Nothing is shown as zero." />
          <table v-else-if="data.failures_by_category.length" class="tbl">
            <thead><tr><th>Category</th><th class="num">Calls</th><th class="share">Share of failures</th></tr></thead>
            <tbody><tr v-for="row in data.failures_by_category" :key="row.category">
              <td>{{ row.category.replace(/_/g, " ") }}</td><td class="num">{{ integer(row.requests) }}</td>
              <td class="share"><span class="share-cell"><span class="meter"><i class="danger" :style="{ width: `${percentOf(row.requests, failureTotal)}%` }" /></span><span class="num">{{ Math.round(percentOf(row.requests, failureTotal)) }}%</span></span></td>
            </tr></tbody>
          </table>
          <div v-else class="empty-state">No failed model calls this month.</div>
        </template>
        <div v-if="(breakdown === 'backend' && !data.by_backend.length) || (breakdown === 'model' && !data.by_model.length)" class="empty-state">No usage has been recorded yet.</div>
      </div>
    </section>
    <p class="usage-source">Cost estimates use {{ data.pricing.source }} (updated {{ data.pricing.updated_at }}). Provider invoices remain authoritative; unknown model prices use conservative accounting. Latency covers only calls with a measured latency.</p>
  </template>
</template>

<style scoped>
.usage-top { display: grid; grid-template-columns: minmax(300px, 1fr) minmax(0, 2fr); gap: 16px; margin-bottom: 16px; }
.budget-card { display: flex; flex-direction: column; gap: 12px; margin: 0; }
.budget-card h2 { margin: 0; }
.budget-figure { display: flex; flex-wrap: wrap; align-items: baseline; gap: 6px 10px; margin: 0; }
.budget-figure .big { font-size: 36px; }
.budget-figure strong { margin-left: auto; font-size: 18px; font-weight: 600; font-variant-numeric: tabular-nums; color: var(--accent-text); }
.budget-figure strong.usage-danger { color: var(--danger-text); }
.budget-meter { height: 10px; border-radius: 5px; }
.budget-lines { display: grid; grid-template-columns: 1fr auto; gap: 6px 12px; margin: 0; }
.chart-card { min-width: 0; }
.chart-card h2 { margin: 0; font-size: 15px; }
.usage-metrics { margin-bottom: 16px; }
.metric-card strong.unmeasured { font-size: 15px; font-weight: 400; color: var(--text-secondary); }
.usage-breakdown { margin-bottom: 8px; }
.usage-breakdown .card-title { align-items: center; padding-bottom: 10px; }
.breakdown-tabs { flex: none; max-width: 100%; }
/* Natural widths: equal shares cut "By backend" short. */
.breakdown-tabs :deep(button) { flex: 0 0 auto; }
.breakdown-unavailable { margin: 14px; }
.share { width: 18%; min-width: 140px; }
.share-cell { display: flex; align-items: center; gap: 10px; }
.share-cell .meter { flex: 1; }
.share-cell .num { min-width: 36px; color: var(--text-secondary); font-size: 13px; text-align: right; }
@media (max-width: 1050px) { .usage-top { grid-template-columns: 1fr; } }
</style>
