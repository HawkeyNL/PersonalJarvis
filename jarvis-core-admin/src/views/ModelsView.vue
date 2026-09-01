<script setup lang="ts">
import { computed, onMounted, ref } from "vue";
import { api, errorText, type ModelRecord, type OperationResult } from "../admin";
import ConfirmDialog from "../components/ConfirmDialog.vue";
import ErrorPanel from "../components/ErrorPanel.vue";
import PageHeader from "../components/PageHeader.vue";
import ResultPanel from "../components/ResultPanel.vue";
import StatusBadge from "../components/StatusBadge.vue";

const rows = ref<ModelRecord[]>([]);
const query = ref("");
const provider = ref("all");
const state = ref("all");
const priceStatus = ref("all");
const maxOutput = ref("");
const sort = ref("provider");
const busy = ref(false);
const error = ref("");
const result = ref<OperationResult | null>(null);
const disableTarget = ref<ModelRecord | null>(null);
const providers = computed(() => [...new Set(rows.value.map((row) => row.provider))].sort());
const filtered = computed(() => {
  const q = query.value.toLowerCase().trim();
  const selected = rows.value.filter((row) =>
    (!q || `${row.provider} ${row.model} ${row.source}`.toLowerCase().includes(q)) &&
    (provider.value === "all" || row.provider === provider.value) &&
    (state.value === "all" || (state.value === "enabled") === row.enabled) &&
    (priceStatus.value === "all" || row.price_status === priceStatus.value) &&
    (!maxOutput.value || (row.output_per_million_usd !== null && row.output_per_million_usd <= Number(maxOutput.value)))
  );
  return selected.sort((a, b) => {
    if (sort.value === "input") return (a.input_per_million_usd ?? Number.POSITIVE_INFINITY) - (b.input_per_million_usd ?? Number.POSITIVE_INFINITY);
    if (sort.value === "output") return (a.output_per_million_usd ?? Number.POSITIVE_INFINITY) - (b.output_per_million_usd ?? Number.POSITIVE_INFINITY);
    return `${a.provider}/${a.model}`.localeCompare(`${b.provider}/${b.model}`);
  });
});
function price(value: number | null): string {
  return value === null ? "—" : `$${value.toLocaleString(undefined, { minimumFractionDigits: 2, maximumFractionDigits: 4 })}`;
}
async function load() {
  busy.value = true; error.value = "";
  try { rows.value = await api.models(); }
  catch (reason) { error.value = errorText(reason); }
  finally { busy.value = false; }
}
async function mutate(action: "refresh" | "enable" | "disable", row?: ModelRecord) {
  busy.value = true; error.value = ""; disableTarget.value = null;
  try {
    const request: Record<string, string> = { action };
    if (row) { request.provider = row.provider; request.model = row.model; }
    result.value = await api.modelMutation(request); await load();
  } catch (reason) { error.value = errorText(reason); }
  finally { busy.value = false; }
}
onMounted(load);
</script>

<template>
  <PageHeader title="Models" description="Exact owner policy with reviewed per-million-token pricing. Unknown prices are never displayed as free." :busy="busy">
    <button class="secondary" @click="load">Refresh view</button><button @click="mutate('refresh')">Refresh catalog</button>
  </PageHeader>
  <ErrorPanel v-if="error" :message="error" /><ResultPanel v-if="result" :result="result" />
  <div class="model-toolbar">
    <input v-model="query" class="search" placeholder="Search provider, model or source…" />
    <select v-model="provider" aria-label="Provider"><option value="all">All providers</option><option v-for="item in providers" :key="item" :value="item">{{ item }}</option></select>
    <select v-model="state" aria-label="State"><option value="all">Any state</option><option value="enabled">Enabled</option><option value="disabled">Disabled</option></select>
    <select v-model="priceStatus" aria-label="Pricing"><option value="all">Any pricing</option><option value="known">Known price</option><option value="unknown">Unknown price</option><option value="local">Local / included</option></select>
    <input v-model="maxOutput" type="number" min="0" step="0.01" placeholder="Max output $/1M" aria-label="Maximum output price per million tokens" />
    <select v-model="sort" aria-label="Sort"><option value="provider">Sort by model</option><option value="input">Cheapest input</option><option value="output">Cheapest output</option></select>
  </div>
  <div class="filter-summary">{{ filtered.length }} of {{ rows.length }} models · prices in USD per 1M tokens</div>
  <section class="table-card"><table><thead><tr><th>Provider</th><th>Model</th><th>State</th><th>Input</th><th>Cached</th><th>Output</th><th>Price</th><th></th></tr></thead>
    <tbody><tr v-for="row in filtered" :key="`${row.provider}/${row.model}`"><td>{{ row.provider }}</td><td class="mono wrap-anywhere">{{ row.model }}</td><td><StatusBadge :state="row.enabled ? 'enabled' : 'disabled'" /></td><td class="mono">{{ price(row.input_per_million_usd) }}</td><td class="mono">{{ price(row.cache_read_per_million_usd) }}</td><td class="mono">{{ price(row.output_per_million_usd) }}</td><td><StatusBadge :state="row.price_status" /></td><td class="table-actions"><button v-if="!row.enabled" class="small secondary" @click="mutate('enable', row)">Enable</button><button v-else class="small danger ghost" @click="disableTarget = row">Disable</button></td></tr></tbody>
  </table><div v-if="!filtered.length && !busy" class="empty-state">No models match these filters.</div></section>
  <p v-if="rows.length" class="usage-source">Pricing source: {{ rows[0].pricing_source }} · updated {{ rows[0].pricing_updated_at }}</p>
  <ConfirmDialog v-if="disableTarget" title="Disable this model?" :detail="`${disableTarget.provider}/${disableTarget.model} will no longer be eligible for routing.`" confirm-label="Disable model" @cancel="disableTarget = null" @confirm="mutate('disable', disableTarget ?? undefined)" />
</template>
