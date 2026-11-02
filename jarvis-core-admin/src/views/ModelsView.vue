<script setup lang="ts">
import { computed, nextTick, onMounted, ref, watch } from "vue";
import { api, errorText, type ModelRecord, type OperationResult } from "../admin";
import ConfirmDialog from "../components/ConfirmDialog.vue";
import ErrorPanel from "../components/ErrorPanel.vue";
import LayaCard from "../components/LayaCard.vue";
import ModelRouting from "../components/ModelRouting.vue";
import PageHeader from "../components/PageHeader.vue";
import ResultPanel from "../components/ResultPanel.vue";
import StatusBadge from "../components/StatusBadge.vue";
import JvSegmented from "../components/jv/JvSegmented.vue";
import HfRoutesDialog from "../components/models/HfRoutesDialog.vue";
import { modelConfirm } from "../components/models/routing";

const TABS = [{ id: "routing", label: "Routing" }, { id: "catalog", label: "Catalog" }, { id: "laya", label: "Local classifier (Laya)" }];
const tab = ref("routing");
const unsaved = ref(0);
const applying = ref(false);
const leaveTo = ref<string | null>(null);
const tabLabel = computed(() => TABS.find((item) => item.id === tab.value)!.label);
// Leaving Routing unmounts it and drops its staged changes, so ask first.
// While an Apply runs the tab stays put: leaving would stop it half-way.
function chooseTab(id: string) {
  if (id === tab.value) return;
  if (applying.value) return stay();
  if (tab.value === "routing" && unsaved.value) leaveTo.value = id;
  else tab.value = id;
}
function leave() { tab.value = leaveTo.value!; leaveTo.value = null; unsaved.value = 0; applying.value = false; }
function stay() {
  leaveTo.value = null;
  // Arrow keys moved focus to the tab that was refused; give it back to Routing.
  void nextTick(() => document.querySelector<HTMLElement>(".models-tabs [aria-selected='true']")?.focus());
}

const rows = ref<ModelRecord[]>([]);
const query = ref("");
const provider = ref("all");
const state = ref("all");
const priceStatus = ref("all");
const maxOutput = ref("");
const sort = ref("provider");
const busy = ref(false);
const error = ref("");
const errorTitle = ref<string>();
const result = ref<OperationResult | null>(null);
const toggleTarget = ref<{ action: "enable" | "disable"; row: ModelRecord } | null>(null);
const selectedHf = ref<ModelRecord | null>(null);
const currentPage = ref(1);
const PAGE_SIZE = 25;
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
// aria-disabled, not disabled: Previous/Next keep focus at the first and last page.
function goPage(page: number) { if (page >= 1 && page <= totalPages.value) currentPage.value = page; }
function price(value: number | null): string {
  return value === null ? "—" : `$${value.toLocaleString(undefined, { minimumFractionDigits: 2, maximumFractionDigits: 4 })}`;
}
async function load() {
  busy.value = true; error.value = "";
  try { rows.value = await api.models(); }
  catch (reason) { error.value = errorText(reason); errorTitle.value = "Could not load models"; }
  finally { busy.value = false; }
}
async function mutate(action: "refresh" | "enable" | "disable", row?: ModelRecord) {
  busy.value = true; error.value = ""; toggleTarget.value = null;
  try {
    const request: Record<string, string> = { action };
    if (action === "refresh" && provider.value !== "all") request.provider = provider.value;
    if (row) { request.provider = row.provider; request.model = row.model; }
    result.value = await api.modelMutation(request); await load();
  } catch (reason) { error.value = errorText(reason); errorTitle.value = undefined; }
  finally { busy.value = false; }
}
const toggleSpec = computed(() => toggleTarget.value && modelConfirm(toggleTarget.value.action, toggleTarget.value.row));
const registerProvider = ref("codex-cli");
const registerModel = ref("");
// Subscriptions have no model catalog: record one exact pair, disabled.
async function registerSubscription() {
  busy.value = true; error.value = "";
  try {
    result.value = await api.modelMutation({ action: "register", provider: registerProvider.value, model: registerModel.value.trim() });
    registerModel.value = ""; await load();
  } catch (reason) { error.value = errorText(reason); errorTitle.value = undefined; }
  finally { busy.value = false; }
}
onMounted(() => { void load(); });
</script>

<template>
  <PageHeader title="Models &amp; routing" description="Exact owner policy with reviewed per-million-token pricing. Unknown prices are never displayed as free." :busy="busy">
    <button class="secondary" @click="load">Refresh view</button><button @click="mutate('refresh')">Refresh catalog</button>
  </PageHeader>
  <ErrorPanel v-if="error" :message="error" :title="errorTitle" /><ResultPanel v-if="result" :result="result" />
  <JvSegmented class="models-tabs" :items="TABS" :model-value="tab" label="Models and routing sections" controls="models-panel" @update:model-value="chooseTab" />
  <div id="models-panel" class="models-panel" role="tabpanel" :aria-label="tabLabel">
    <ModelRouting v-if="tab === 'routing'" :models="rows" @changes="(count: number) => (unsaved = count)" @applying="(active: boolean) => (applying = active)" />
    <!-- Stays mounted, as before the tabs: a long Laya install keeps its state and result. -->
    <div v-show="tab === 'laya'"><LayaCard /></div>
    <template v-if="tab === 'catalog'">
      <form class="model-toolbar register-form" @submit.prevent="registerSubscription">
        <select v-model="registerProvider" aria-label="Subscription provider"><option value="claude-cli">claude-cli</option><option value="codex-cli">codex-cli</option></select>
        <input v-model="registerModel" class="search" maxlength="80" required placeholder="Subscription model, e.g. gpt-6-luna" aria-label="Subscription model" />
        <button class="secondary" :disabled="busy">Register subscription model</button>
      </form>
      <p class="usage-source">Registering records the exact pair as disabled; enable it below before routing uses it.</p>
      <div class="model-toolbar" role="search" aria-label="Filter models">
        <label class="sr-only" for="model-search">Search models</label>
        <input id="model-search" v-model="query" class="search" placeholder="Search provider, model or source…" />
        <select v-model="provider" aria-label="Provider"><option value="all">All providers</option><option v-for="item in providers" :key="item" :value="item">{{ item }}</option></select>
        <select v-model="state" aria-label="State"><option value="all">Any state</option><option value="enabled">Enabled</option><option value="disabled">Disabled</option></select>
        <select v-model="priceStatus" aria-label="Pricing"><option value="all">Any pricing</option><option value="known">Known price</option><option value="estimated">Estimated</option><option value="conservative">Conservative</option><option value="unknown">Unknown price</option><option value="local">Local / included</option></select>
        <input v-model="maxOutput" type="number" min="0" step="0.01" placeholder="Max output $/1M" aria-label="Maximum output price per million tokens" />
        <select v-model="sort" aria-label="Sort"><option value="provider">Sort by model</option><option value="input">Cheapest input</option><option value="output">Cheapest output</option></select>
      </div>
      <div class="filter-summary" role="status">{{ filtered.length }} of {{ rows.length }} models · showing {{ firstVisible }}–{{ lastVisible }} · prices in USD per 1M tokens</div>
      <section class="table-card" aria-label="Model catalog"><table><thead><tr><th>Provider</th><th>Model</th><th>State</th><th>HF route</th><th>Input</th><th>Cached</th><th>Output</th><th>Price</th><th>Actions</th></tr></thead>
        <tbody><tr v-for="row in pageRows" :key="`${row.provider}/${row.model}`"><td>{{ row.provider }}</td><td class="mono wrap-anywhere">{{ row.model }}<details v-if="row.pricing_source"><summary>Pricing details</summary><p class="usage-source wrap-anywhere">{{ row.pricing_source }} · {{ row.pricing_updated_at }}</p><p class="usage-source wrap-anywhere">{{ row.pricing_notes || "Owner-configured rates. Check applicable context and service tier." }}</p></details></td><td><StatusBadge :state="row.enabled ? 'enabled' : 'disabled'" /></td><td class="mono">{{ row.provider === 'huggingface' ? (row.route ?? 'tier default') : '—' }}</td><td class="mono">{{ price(row.input_per_million_usd) }}</td><td class="mono">{{ price(row.cache_read_per_million_usd) }}</td><td class="mono">{{ price(row.output_per_million_usd) }}</td><td><StatusBadge :state="row.price_status" /></td><td class="table-actions"><button v-if="row.provider === 'huggingface'" class="small secondary" :aria-label="`Routes for ${row.model}`" @click="selectedHf = row">Routes</button><button v-if="!row.enabled" class="small secondary" :disabled="busy" :aria-label="`Enable ${row.provider}/${row.model}`" @click="toggleTarget = { action: 'enable', row }">Enable…</button><button v-else class="small danger ghost" :disabled="busy" :aria-label="`Disable ${row.provider}/${row.model}`" @click="toggleTarget = { action: 'disable', row }">Disable…</button></td></tr></tbody>
      </table><div v-if="!filtered.length && !busy" class="empty-state">No models match these filters.</div></section>
      <nav v-if="filtered.length > PAGE_SIZE" class="pagination" aria-label="Model pages">
        <button class="btn small secondary" :aria-disabled="currentPage === 1" @click="goPage(currentPage - 1)">Previous</button>
        <button v-for="page in visiblePages" :key="page" class="small secondary page-number" :class="{ active: currentPage === page }" :aria-label="`Page ${page}`" :aria-current="currentPage === page ? 'page' : undefined" @click="goPage(page)">{{ page }}</button>
        <button class="btn small secondary" :aria-disabled="currentPage === totalPages" @click="goPage(currentPage + 1)">Next</button>
        <span>Page {{ currentPage }} of {{ totalPages }}</span>
      </nav>
      <p v-if="rows.length" class="usage-source">USD per million text tokens, not an invoice. Expand a model's pricing details for its source, review date and context/peak-rate conditions. Unknown cache prices are not assumed to be discounted.</p>
    </template>
  </div>
  <HfRoutesDialog v-if="selectedHf" :key="`${selectedHf.provider}/${selectedHf.model}`" :row="selectedHf" @close="selectedHf = null" @rows="(next: ModelRecord[]) => (rows = next)" />
  <ConfirmDialog v-if="toggleTarget && toggleSpec" :title="toggleSpec.title" :detail="toggleSpec.detail" :confirm-label="toggleSpec.label" :tone="toggleSpec.tone" @cancel="toggleTarget = null" @confirm="mutate(toggleTarget.action, toggleTarget.row)" />
  <ConfirmDialog v-if="leaveTo" title="Discard unsaved routing changes?" :detail="`Routing has ${unsaved} unsaved ${unsaved === 1 ? 'change' : 'changes'}. Leaving this tab drops what you have not applied; nothing was sent to Core.`" confirm-label="Discard changes" tone="danger" @cancel="stay" @confirm="leave" />
</template>

<style scoped>
.models-tabs { margin-bottom: 16px; }
/* The global hover and press rules only exclude :disabled. */
.btn[aria-disabled="true"]:hover, .btn[aria-disabled="true"]:active { background: var(--bg-sunken); transform: none; }
</style>
