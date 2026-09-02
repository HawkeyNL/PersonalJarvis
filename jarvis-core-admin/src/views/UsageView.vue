<script setup lang="ts">
import { computed, onMounted, ref } from "vue";
import { api, errorText, type UsageReport } from "../admin";
import DailyUsageChart from "../components/DailyUsageChart.vue";
import ErrorPanel from "../components/ErrorPanel.vue";
import PageHeader from "../components/PageHeader.vue";

const data = ref<UsageReport | null>(null);
const busy = ref(false);
const error = ref("");
const maxBackend = computed(() => Math.max(1, ...(data.value?.by_backend.map((row) => row.total_tokens) ?? [1])));
const budgetPercent = computed(() => {
  if (!data.value || data.value.budget_eur <= 0) return 0;
  return Math.min(100, (data.value.spent_eur / data.value.budget_eur) * 100);
});

function integer(value: number): string { return new Intl.NumberFormat().format(value); }
function eur(value: number): string { return new Intl.NumberFormat(undefined, { style: "currency", currency: "EUR" }).format(value); }
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
  <PageHeader title="Usage & Costs" description="Bounded monthly token and cost telemetry. Prompts, replies, credentials and request identifiers never enter this view." :busy="busy">
    <button class="secondary" @click="load">Refresh</button>
  </PageHeader>
  <ErrorPanel v-if="error" :message="error" />
  <template v-if="data">
    <section class="usage-metrics">
      <article class="metric-card"><span class="card-label">TOTAL TOKENS</span><strong>{{ integer(data.total_tokens) }}</strong><small>{{ integer(data.requests) }} model calls</small></article>
      <article class="metric-card"><span class="card-label">INPUT</span><strong>{{ integer(data.input_tokens) }}</strong><small>{{ integer(data.cache_read_tokens) }} cached</small></article>
      <article class="metric-card"><span class="card-label">OUTPUT</span><strong>{{ integer(data.output_tokens) }}</strong><small>{{ integer(data.cache_write_tokens) }} cache writes</small></article>
      <article class="metric-card"><span class="card-label">MONTH SPEND</span><strong>{{ eur(data.spent_eur) }}</strong><small>{{ eur(data.remaining_eur) }} remaining</small></article>
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

    <section class="table-card usage-models">
      <div class="table-title"><div><span class="card-label">MODEL BREAKDOWN</span><small>Current calendar month</small></div><small>Pricing {{ data.pricing.updated_at }}</small></div>
      <table><thead><tr><th>Provider</th><th>Model</th><th>Calls</th><th>Tokens</th><th>Spend</th></tr></thead>
        <tbody><tr v-for="row in data.by_model" :key="`${row.backend}/${row.model}`"><td>{{ row.backend }}</td><td class="mono wrap-anywhere">{{ row.model }}</td><td>{{ integer(row.requests) }}</td><td>{{ integer(row.total_tokens) }}</td><td>{{ eur(row.spent_eur) }}</td></tr></tbody>
      </table>
      <div v-if="!data.by_model.length" class="empty-state">No model usage has been recorded yet.</div>
    </section>
    <p class="usage-source">Cost estimates use {{ data.pricing.source }}. Provider invoices remain authoritative; unknown model prices use conservative accounting.</p>
  </template>
</template>
