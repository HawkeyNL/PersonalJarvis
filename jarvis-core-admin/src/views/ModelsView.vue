<script setup lang="ts">
import { computed, onBeforeUnmount, onMounted, ref, watch } from "vue";
import { api, errorText, type HfProvidersResponse, type ModelRecord, type OperationResult } from "../admin";
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
const selectedHf = ref<ModelRecord | null>(null);
const hfDetails = ref<HfProvidersResponse | null>(null);
const selectedRoute = ref("fastest");
const routeLoading = ref(false);
const routeSaving = ref(false);
const routeBusy = computed(() => routeLoading.value || routeSaving.value);
const routeError = ref("");
const routeResult = ref<OperationResult | null>(null);
const currentPage = ref(1);
const PAGE_SIZE = 25;
let routeRequest = 0;
const providers = computed(() => [...new Set(rows.value.map((row) => row.provider))].sort());
const filtered = computed(() => {
  const q = query.value.toLowerCase().trim();
  const selected = rows.value.filter((row) =>
    (!q || `${row.provider} ${row.model} ${row.route ?? ""} ${row.source}`.toLowerCase().includes(q)) &&
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
const totalPages = computed(() => Math.max(1, Math.ceil(filtered.value.length / PAGE_SIZE)));
const pageRows = computed(() => {
  const start = (currentPage.value - 1) * PAGE_SIZE;
  return filtered.value.slice(start, start + PAGE_SIZE);
});
const visiblePages = computed(() => {
  const end = Math.min(totalPages.value, Math.max(5, currentPage.value + 2));
  const start = Math.max(1, end - 4);
  return Array.from({ length: end - start + 1 }, (_, index) => start + index);
});
const firstVisible = computed(() => filtered.value.length ? (currentPage.value - 1) * PAGE_SIZE + 1 : 0);
const lastVisible = computed(() => Math.min(currentPage.value * PAGE_SIZE, filtered.value.length));
watch([query, provider, state, priceStatus, maxOutput, sort], () => { currentPage.value = 1; });
watch(totalPages, (pages) => { currentPage.value = Math.min(currentPage.value, pages); });
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
    if (action === "refresh" && provider.value !== "all") request.provider = provider.value;
    if (row) { request.provider = row.provider; request.model = row.model; }
    result.value = await api.modelMutation(request); await load();
  } catch (reason) { error.value = errorText(reason); }
  finally { busy.value = false; }
}
async function inspectHf(row: ModelRecord) {
  if (row.provider !== "huggingface") return;
  const request = ++routeRequest;
  routeLoading.value = true; routeError.value = ""; routeResult.value = null; selectedHf.value = row; hfDetails.value = null;
  try {
    const details = await api.modelProviders(row.model);
    if (request === routeRequest) { hfDetails.value = details; selectedRoute.value = row.route ?? "auto"; }
  } catch (reason) { if (request === routeRequest) routeError.value = errorText(reason); }
  finally { if (request === routeRequest) routeLoading.value = false; }
}
async function saveRoute() {
  const row = selectedHf.value;
  if (!row) return;
  routeSaving.value = true; routeError.value = ""; routeResult.value = null;
  try {
    const operation = await api.modelMutation({ action: "set_route", provider: row.provider, model: row.model, route: selectedRoute.value });
    rows.value = await api.models();
    selectedHf.value = rows.value.find((item) => item.provider === row.provider && item.model === row.model) ?? row;
    hfDetails.value = await api.modelProviders(row.model);
    selectedRoute.value = selectedHf.value.route ?? "auto";
    routeResult.value = operation;
  } catch (reason) { routeError.value = errorText(reason); }
  finally { routeSaving.value = false; }
}
function closeRoutes() {
  if (routeSaving.value) return;
  routeRequest++; routeLoading.value = false;
  selectedHf.value = null; hfDetails.value = null; routeError.value = ""; routeResult.value = null;
}
function onKeydown(event: KeyboardEvent) {
  if (event.key === "Escape" && selectedHf.value) closeRoutes();
}
onMounted(() => { void load(); window.addEventListener("keydown", onKeydown); });
onBeforeUnmount(() => window.removeEventListener("keydown", onKeydown));
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
    <select v-model="priceStatus" aria-label="Pricing"><option value="all">Any pricing</option><option value="known">Known price</option><option value="estimated">Estimated</option><option value="conservative">Conservative</option><option value="unknown">Unknown price</option><option value="local">Local / included</option></select>
    <input v-model="maxOutput" type="number" min="0" step="0.01" placeholder="Max output $/1M" aria-label="Maximum output price per million tokens" />
    <select v-model="sort" aria-label="Sort"><option value="provider">Sort by model</option><option value="input">Cheapest input</option><option value="output">Cheapest output</option></select>
  </div>
  <div class="filter-summary">{{ filtered.length }} of {{ rows.length }} models · showing {{ firstVisible }}–{{ lastVisible }} · prices in USD per 1M tokens</div>
  <section class="table-card"><table><thead><tr><th>Provider</th><th>Model</th><th>State</th><th>HF route</th><th>Input</th><th>Cached</th><th>Output</th><th>Price</th><th></th></tr></thead>
    <tbody><tr v-for="row in pageRows" :key="`${row.provider}/${row.model}`"><td>{{ row.provider }}</td><td class="mono wrap-anywhere">{{ row.model }}</td><td><StatusBadge :state="row.enabled ? 'enabled' : 'disabled'" /></td><td class="mono">{{ row.provider === 'huggingface' ? (row.route ?? 'tier default') : '—' }}</td><td class="mono">{{ price(row.input_per_million_usd) }}</td><td class="mono">{{ price(row.cache_read_per_million_usd) }}</td><td class="mono">{{ price(row.output_per_million_usd) }}</td><td><StatusBadge :state="row.price_status" /></td><td class="table-actions"><button v-if="row.provider === 'huggingface'" class="small secondary" @click="inspectHf(row)">Routes</button><button v-if="!row.enabled" class="small secondary" @click="mutate('enable', row)">Enable</button><button v-else class="small danger ghost" @click="disableTarget = row">Disable</button></td></tr></tbody>
  </table><div v-if="!filtered.length && !busy" class="empty-state">No models match these filters.</div></section>
  <nav v-if="filtered.length > PAGE_SIZE" class="pagination" aria-label="Model pages">
    <button class="small secondary" :disabled="currentPage === 1" @click="currentPage--">Previous</button>
    <button v-for="page in visiblePages" :key="page" class="small secondary page-number" :class="{ active: currentPage === page }" :aria-current="currentPage === page ? 'page' : undefined" @click="currentPage = page">{{ page }}</button>
    <button class="small secondary" :disabled="currentPage === totalPages" @click="currentPage++">Next</button>
    <span>Page {{ currentPage }} of {{ totalPages }}</span>
  </nav>
  <p v-if="rows.length" class="usage-source">Pricing source: {{ rows[0].pricing_source }} · updated {{ rows[0].pricing_updated_at }}</p>
  <div v-if="selectedHf" class="dialog-backdrop" @click.self="closeRoutes">
    <section class="dialog routes-dialog" role="dialog" aria-modal="true" aria-labelledby="routes-title">
      <div class="card-heading"><div><span class="card-label">HUGGING FACE INFERENCE PROVIDERS</span><h2 id="routes-title" class="mono wrap-anywhere">{{ selectedHf.model }}</h2></div><button class="small secondary" :disabled="routeSaving" @click="closeRoutes">Close</button></div>
      <ErrorPanel v-if="routeError" :message="routeError" /><ResultPanel v-if="routeResult" :result="routeResult" />
      <div v-if="routeLoading && !hfDetails" class="modal-loading"><span class="spinner" />Loading available routes…</div>
      <template v-if="hfDetails">
        <div class="route-controls"><select v-model="selectedRoute" :disabled="routeBusy" aria-label="Hugging Face route"><option v-for="route in hfDetails.routes" :key="route" :value="route">{{ route }}</option></select><button :disabled="routeBusy" @click="saveRoute">{{ routeSaving ? 'Saving…' : 'Save route' }}</button></div>
        <p class="usage-source">Route selection does not enable this model. Dynamic routes use conservative budget estimates when the execution provider is not known.</p>
        <div class="table-card"><table><thead><tr><th>Route</th><th>Status</th><th>Context</th><th>Input/M</th><th>Output/M</th><th>TTFT</th><th>Tok/s</th><th>Tools</th><th>Structured</th></tr></thead><tbody>
          <tr v-for="item in hfDetails.providers" :key="item.provider"><td class="mono">{{ item.provider }}</td><td><StatusBadge :state="item.status" /></td><td class="mono">{{ item.context_length ?? '—' }}</td><td class="mono">{{ price(item.input_per_million_usd) }}</td><td class="mono">{{ price(item.output_per_million_usd) }}</td><td class="mono">{{ item.first_token_latency_ms === null ? '—' : `${item.first_token_latency_ms} ms` }}</td><td class="mono">{{ item.throughput ?? '—' }}</td><td>{{ item.supports_tools === null ? '—' : (item.supports_tools ? 'yes' : 'no') }}</td><td>{{ item.supports_structured_output === null ? '—' : (item.supports_structured_output ? 'yes' : 'no') }}</td></tr>
        </tbody></table></div>
      </template>
    </section>
  </div>
  <ConfirmDialog v-if="disableTarget" title="Disable this model?" :detail="`${disableTarget.provider}/${disableTarget.model} will no longer be eligible for routing.`" confirm-label="Disable model" @cancel="disableTarget = null" @confirm="mutate('disable', disableTarget ?? undefined)" />
</template>
