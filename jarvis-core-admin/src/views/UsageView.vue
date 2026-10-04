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
const maxBackend = computed(() => Math.max(1, ...(data.value?.by_backend.map((row) => row.total_tokens) ?? [1])));
const maxFailures = computed(() => Math.max(1, ...(data.value?.failures_by_category?.map((row) => row.requests) ?? [1])));
const budgetPercent = computed(() => (data.value ? percentOf(data.value.spent_eur, data.value.budget_eur) : 0));

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
  <PageHeader title="Usage & Costs" description="Bounded monthly token, cost, latency and reliability aggregates. Prompts, replies, credentials and request identifiers never enter this view." :busy="busy">
    <button class="secondary" @click="load">Refresh</button>
  </PageHeader>
  <ErrorPanel v-if="error && !initializing" :message="error" />
  <section v-if="initializing" class="detail-card empty-state">
    Usage statistics are initializing. Core retries the safe aggregate automatically; refresh this view in a moment.
  </section>
  <template v-if="data">
    <section class="usage-metrics">
      <article class="metric-card"><span class="card-label">TOTAL TOKENS</span><strong>{{ integer(data.total_tokens) }}</strong><small>{{ integer(data.requests) }} model calls</small></article>
      <article class="metric-card"><span class="card-label">INPUT</span><strong>{{ integer(data.input_tokens) }}</strong><small>{{ integer(data.cache_read_tokens) }} cached</small></article>
      <article class="metric-card"><span class="card-label">OUTPUT</span><strong>{{ integer(data.output_tokens) }}</strong><small>{{ integer(data.cache_write_tokens) }} cache writes</small></article>
      <article class="metric-card"><span class="card-label">MONTH SPEND</span><strong>{{ eur(data.spent_eur) }}</strong><small>{{ eur(data.remaining_eur) }} remaining</small></article>
      <article class="metric-card"><span class="card-label">LATENCY P50</span><strong :class="{ unmeasured: data.latency_p50_ms === null }">{{ measured(data.latency_p50_ms, ms) }}</strong><small>Calls with a measured latency</small></article>
      <article class="metric-card"><span class="card-label">LATENCY P95</span><strong :class="{ unmeasured: data.latency_p95_ms === null }">{{ measured(data.latency_p95_ms, ms) }}</strong><small>Slowest 5% start above this</small></article>
      <article class="metric-card"><span class="card-label">FAILURES</span><strong :class="{ unmeasured: data.failures === null }">{{ measured(data.failures) }}</strong><small>Failed model calls this month</small></article>
      <article class="metric-card"><span class="card-label">FALLBACKS</span><strong :class="{ unmeasured: data.fallbacks === null }">{{ measured(data.fallbacks) }}</strong><small>Router moves to a next model</small></article>
    </section>

    <section class="detail-card budget-card">
      <div class="budget-heading"><div><span class="card-label">MONTHLY HARD BUDGET</span><h2>{{ eur(data.spent_eur) }} / {{ eur(data.budget_eur) }}</h2></div><strong :class="{ 'usage-danger': data.over_budget }">{{ budgetPercent.toFixed(0) }}%</strong></div>
      <div class="usage-progress"><i :class="{ danger: data.over_budget }" :style="{ width: `${budgetPercent}%` }" /></div>
      <small>{{ eur(data.reserved_eur) }} reserved · {{ eur(data.remaining_hard_eur) }} hard capacity remaining</small>
    </section>

    <section class="usage-grid">
      <article class="detail-card">
        <span class="card-label">TOKENS BY DAY</span>
        <DailyUsageChart v-if="data.daily.length" :rows="data.daily" />
        <div v-else class="empty-state">No recorded model calls this month.</div>
      </article>
      <article class="detail-card">
        <span class="card-label">BY PROVIDER</span>
        <div v-if="data.by_backend.length" class="provider-usage">
          <div v-for="row in data.by_backend" :key="row.backend">
            <div><strong>{{ row.backend }}</strong><span>{{ integer(row.total_tokens) }} · {{ eur(row.spent_eur) }}</span></div>
            <div class="usage-progress slim"><i :style="{ width: `${row.total_tokens / maxBackend * 100}%` }" /></div>
          </div>
        </div>
        <div v-else class="empty-state">No provider telemetry yet.</div>
      </article>
    </section>

    <JvSegmented v-model="breakdown" class="breakdown-tabs" :items="BREAKDOWNS" label="Usage breakdown" controls="usage-breakdown" />
    <section id="usage-breakdown" role="tabpanel" class="table-card usage-models">
      <table v-if="breakdown === 'backend'">
        <thead><tr><th>Backend</th><th class="num">Calls</th><th class="num">Tokens</th><th class="num">Spend</th><th class="num">Failures</th><th class="num">Fallbacks</th><th class="num">p50</th><th class="num">p95</th></tr></thead>
        <tbody><tr v-for="row in data.by_backend" :key="row.backend">
          <td>{{ row.backend }}</td><td class="num">{{ integer(row.requests) }}</td><td class="num">{{ integer(row.total_tokens) }}</td><td class="num">{{ eur(row.spent_eur) }}</td>
          <td v-for="(item, index) in [cell(row.failures), cell(row.fallbacks), cell(row.latency_p50_ms, ms), cell(row.latency_p95_ms, ms)]" :key="index" class="num" :class="{ unmeasured: item.unmeasured }">{{ item.text }}</td>
        </tr></tbody>
      </table>
      <table v-else-if="breakdown === 'model'">
        <thead><tr><th>Provider</th><th>Model</th><th class="num">Calls</th><th class="num">Tokens</th><th class="num">Spend</th><th class="num">Failures</th><th class="num">Fallbacks</th></tr></thead>
        <tbody><tr v-for="row in data.by_model" :key="`${row.backend}/${row.model}`">
          <td>{{ row.backend }}</td><td class="mono wrap-anywhere">{{ row.model }}</td><td class="num">{{ integer(row.requests) }}</td><td class="num">{{ integer(row.total_tokens) }}</td><td class="num">{{ eur(row.spent_eur) }}</td>
          <td v-for="(item, index) in [cell(row.failures), cell(row.fallbacks)]" :key="index" class="num" :class="{ unmeasured: item.unmeasured }">{{ item.text }}</td>
        </tr></tbody>
      </table>
      <template v-else-if="breakdown === 'agent'">
        <JvUnavailable v-if="data.by_agent === null" class="breakdown-unavailable" icon="agents" title="Usage per agent" detail="This Core does not record which agent made a model call yet, or its release predates per-agent usage. Nothing is shown as zero." />
        <table v-else>
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
        <div v-else-if="data.failures_by_category.length" class="provider-usage failure-usage">
          <div v-for="row in data.failures_by_category" :key="row.category">
            <div><strong>{{ row.category.replace(/_/g, " ") }}</strong><span>{{ integer(row.requests) }} calls</span></div>
            <div class="usage-progress slim"><i class="danger" :style="{ width: `${row.requests / maxFailures * 100}%` }" /></div>
          </div>
        </div>
        <div v-else class="empty-state">No failed model calls this month.</div>
      </template>
      <div v-if="(breakdown === 'backend' && !data.by_backend.length) || (breakdown === 'model' && !data.by_model.length)" class="empty-state">No usage has been recorded yet.</div>
    </section>
    <p class="usage-source">Cost estimates use {{ data.pricing.source }} (updated {{ data.pricing.updated_at }}). Provider invoices remain authoritative; unknown model prices use conservative accounting. Latency covers only calls with a measured latency.</p>
  </template>
</template>

<style scoped>
.breakdown-tabs { margin: 4px 0 12px; }
.metric-card strong.unmeasured { font-family: var(--font-body); font-size: var(--fs-15); font-style: italic; font-weight: 300; color: var(--idle); }
.breakdown-unavailable { margin: 14px; }
.failure-usage { margin: 0; padding: 16px; }
</style>
