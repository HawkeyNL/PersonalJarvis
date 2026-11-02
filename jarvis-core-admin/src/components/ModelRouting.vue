<script setup lang="ts">
import { computed, onBeforeUnmount, onMounted, reactive, ref, watch } from "vue";
import { api, errorText, type ModelRecord, type OperationResult, type RouteTier, type RoutingReport } from "../admin";
import ConfirmDialog from "./ConfirmDialog.vue";
import ErrorPanel from "./ErrorPanel.vue";
import ResultPanel from "./ResultPanel.vue";
import { MAX_CHAIN, ROUTE_PROVIDERS, TIERS, applyConfirm, applyInOrder, baseline, changes, entryKey as key, kind, paidAfterSubscription, restage, stageConfirm, type ConfirmSpec, type Staged } from "./models/routing";

const props = defineProps<{ models: ModelRecord[] }>();
const emit = defineEmits<{ changes: [count: number]; applying: [active: boolean] }>();
const REASONS: Record<string, string> = {
  routing_invalid: "routing.json is invalid.",
  routing_unsafe: "routing.json has unsafe ownership or permissions, or is a link.",
  routing_unreadable: "routing.json could not be read.",
  routing_too_large: "routing.json is larger than 64 KiB.",
};

const report = ref<RoutingReport | null>(null);
// Edits stay here, in memory, until Apply; a lock or leaving the page unmounts
// this component and drops them.
const staged = ref<Staged | null>(null);
const picks = reactive<Record<RouteTier, string>>({ cheap: "", default: "", hard: "" });
const busy = ref(false);
const progress = ref("");
const error = ref("");
const errorTitle = ref<string>();
const results = ref<OperationResult[]>([]);
type Pending = { kind: "allow_paid" | "research_on" | "reset" | "apply"; tier?: RouteTier; spec: ConfirmSpec };
const pending = ref<Pending | null>(null);
let alive = true;

const candidates = computed(() => props.models.filter((row) => ROUTE_PROVIDERS.includes(row.provider)));
const savedPaid = computed(() => report.value?.routing?.paid_api ?? "allowed");
const savedResearch = computed(() => report.value?.routing?.research_web_search ?? "off");
const unavailable = computed(() => report.value?.routing_unavailable_reason ?? null);
// The CLI refuses to edit an unusable file; it has to be repaired as root first.
const locked = computed(() => busy.value || !report.value || !staged.value || !!unavailable.value);
const list = computed(() => (report.value && staged.value ? changes(report.value, staged.value) : []));
const problems = computed(() => list.value.flatMap((change) => change.problem ?? []));
const changedKeys = computed(() => new Set(list.value.map((change) => change.key)));
watch(() => list.value.length, (count) => emit("changes", count), { immediate: true });
watch(progress, (text) => emit("applying", !!text));

const stored = (tier: RouteTier) => report.value?.routing?.tiers[tier] ?? null;
const enabled = (model: string) => props.models.some((row) => key(row) === model && row.enabled);
const label = (tier: RouteTier) => TIERS.find((item) => item.tier === tier)!.label;

function add(tier: RouteTier) {
  const draft = staged.value!.tiers;
  const row = candidates.value.find((item) => key(item) === picks[tier]);
  const chain = draft[tier]?.chain ?? [];
  if (row && chain.length < MAX_CHAIN && !chain.some((entry) => key(entry) === picks[tier])) {
    draft[tier] = { chain: [...chain, { provider: row.provider, model: row.model }], metered_after_subscription: draft[tier]?.metered_after_subscription ?? false };
  }
  picks[tier] = "";
}
function move(tier: RouteTier, index: number, delta: number) {
  const chain = staged.value!.tiers[tier]!.chain;
  [chain[index], chain[index + delta]] = [chain[index + delta], chain[index]];
}
function confirm(kind: Pending["kind"], tier?: RouteTier) {
  pending.value = { kind, tier, spec: kind === "apply" ? applyConfirm(list.value) : stageConfirm(kind, tier ? label(tier) : "") };
}
function confirmed() {
  const action = pending.value!;
  pending.value = null;
  if (action.kind === "apply") return void apply();
  const draft = staged.value!;
  if (action.kind === "allow_paid") draft.paid_api = "allowed";
  else if (action.kind === "research_on") draft.research_web_search = "on";
  else draft.tiers[action.tier!] = null;
}
// Turning on asks first, unless it only undoes a staged "off". One button, so
// focus comes back to it after the confirm.
function togglePaid() {
  const draft = staged.value!;
  if (draft.paid_api === "allowed") draft.paid_api = "off";
  else if (savedPaid.value === "allowed") draft.paid_api = "allowed";
  else confirm("allow_paid");
}
function toggleResearch() {
  const draft = staged.value!;
  if (draft.research_web_search === "on") draft.research_web_search = "off";
  else if (savedResearch.value === "on") draft.research_web_search = "on";
  else confirm("research_on");
}
function discard() { if (report.value) staged.value = baseline(report.value); }

async function fetchReport() { report.value = await api.modelRoutes(); }
async function load() {
  busy.value = true; error.value = "";
  try { await fetchReport(); staged.value = baseline(report.value!); }
  catch (reason) { error.value = errorText(reason); errorTitle.value = "Could not load model routing"; }
  finally { busy.value = false; }
}
// One broker call per change, in page order, as the separate buttons did.
async function apply() {
  const todo = list.value;
  if (busy.value || !todo.length || problems.value.length) return;
  const draft = staged.value!;
  busy.value = true; error.value = ""; results.value = [];
  progress.value = `Applying 1 of ${todo.length}…`;
  const outcome = await applyInOrder(todo, api.modelRouteMutation, () => alive, (result, index) => {
    results.value.push(result);
    progress.value = `Applying ${Math.min(index + 2, todo.length)} of ${todo.length}…`;
  });
  if (!alive) return;
  if (outcome.error !== undefined) { error.value = errorText(outcome.error); errorTitle.value = `Could not apply: ${todo[outcome.applied].summary}`; }
  // Reload either way; a failed reload never replaces the apply error.
  try {
    await fetchReport();
    staged.value = restage(report.value!, draft, todo.slice(outcome.applied).map((change) => change.key));
  } catch (reason) {
    // Core's state is unknown now: show nothing editable until it loads again.
    report.value = null; staged.value = null;
    if (outcome.error === undefined) { error.value = errorText(reason); errorTitle.value = "Could not load model routing"; }
  } finally { busy.value = false; progress.value = ""; }
}
onMounted(() => { void load(); });
onBeforeUnmount(() => { alive = false; });
</script>

<template>
  <section class="detail-card routing-card" aria-labelledby="routing-title">
    <div class="card-heading">
      <div><span class="card-label">Model routing</span><h2 id="routing-title">Order per tier</h2></div>
      <div v-if="staged" class="paid-switch" :class="staged.paid_api">
        <span>Paid APIs: <strong>{{ staged.paid_api }}</strong><template v-if="staged.paid_api !== savedPaid"> (not applied)</template></span>
        <button class="small secondary" :disabled="locked" @click="togglePaid">{{ staged.paid_api === "allowed" ? "Turn off" : "Allow" }}</button>
      </div>
    </div>
    <div v-if="staged" class="paid-switch" :class="staged.research_web_search === 'on' ? 'allowed' : 'off'">
      <span>Research web search: <strong>{{ staged.research_web_search }}</strong><template v-if="staged.research_web_search !== savedResearch"> (not applied)</template></span>
      <button class="small secondary" :disabled="locked" @click="toggleResearch">{{ staged.research_web_search === "on" ? "Turn off" : "Turn on" }}</button>
    </div>
    <p class="usage-source">Research web search lets an explicit Research request search the web through an enabled subscription in the Hard tier (claude-cli, codex-cli), never a paid API. Only the question leaves the Home Node, to that provider and its search partners.</p>
    <p class="usage-source">Paid APIs off keeps only subscriptions (claude-cli, codex-cli) and local Ollama in every tier. Routing only orders models: a model still has to be enabled, and disabled models are skipped. Changes wait here until you apply them; applying restarts Jarvis Core.</p>
    <ErrorPanel v-if="unavailable" title="Routing file needs repair" :message="`${REASONS[unavailable] ?? 'Routing is unavailable.'} Jarvis uses the built-in order without paid APIs until it is fixed as root (${unavailable}).`" />
    <ErrorPanel v-if="error" :message="error" :title="errorTitle" />
    <button v-if="!report && !busy" class="small secondary" @click="load">Load routing again</button>
    <ResultPanel v-for="(item, index) in results" :key="index" :result="item" />
    <div v-if="report && staged" class="routing-grid">
      <div v-for="item in TIERS" :key="item.tier" class="routing-tier">
        <span class="card-label">{{ item.label }} · {{ stored(item.tier) ? "owner order" : "built-in order" }}<template v-if="changedKeys.has(item.tier)"> · changed</template></span>
        <ol v-if="staged.tiers[item.tier]?.chain.length" class="route-chain">
          <li v-for="(entry, index) in staged.tiers[item.tier]!.chain" :key="key(entry)">
            <span class="mono wrap-anywhere">{{ entry.provider }} / {{ entry.model }}</span>
            <small>{{ kind(entry.provider) }}{{ enabled(key(entry)) ? "" : " · not enabled, skipped" }}</small>
            <span class="action-row">
              <button class="small secondary" :disabled="locked || index === 0" :aria-label="`Move ${key(entry)} up in ${item.label}`" @click="move(item.tier, index, -1)">↑</button>
              <button class="small secondary" :disabled="locked || index === staged.tiers[item.tier]!.chain.length - 1" :aria-label="`Move ${key(entry)} down in ${item.label}`" @click="move(item.tier, index, 1)">↓</button>
              <button class="small danger ghost" :disabled="locked" :aria-label="`Remove ${key(entry)} from ${item.label}`" @click="staged.tiers[item.tier]!.chain.splice(index, 1)">✕</button>
            </span>
          </li>
        </ol>
        <p v-else-if="staged.tiers[item.tier]" class="route-warning">Empty order: add a model or use the built-in order.</p>
        <p v-else-if="stored(item.tier)" class="usage-source">Your order is removed when you apply; Jarvis then uses its built-in order for this tier.</p>
        <p v-else class="usage-source">No owner order: Jarvis uses its built-in order for this tier.</p>
        <div class="route-controls">
          <select v-model="picks[item.tier]" :disabled="locked || (staged.tiers[item.tier]?.chain.length ?? 0) >= MAX_CHAIN" :aria-label="`Add a model to ${item.label}`">
            <option value="">{{ (staged.tiers[item.tier]?.chain.length ?? 0) >= MAX_CHAIN ? `At most ${MAX_CHAIN} models` : "Add a discovered model…" }}</option>
            <option v-for="row in candidates" :key="key(row)" :value="key(row)" :disabled="staged.tiers[item.tier]?.chain.some((entry) => key(entry) === key(row))">{{ row.provider }} / {{ row.model }}{{ row.enabled ? "" : " (disabled)" }}</option>
          </select>
          <button class="small secondary" :disabled="locked || !picks[item.tier]" :aria-label="`Add to ${item.label}`" @click="add(item.tier)">Add</button>
        </div>
        <template v-if="staged.tiers[item.tier]">
          <label class="route-check"><input v-model="staged.tiers[item.tier]!.metered_after_subscription" type="checkbox" :disabled="locked" />Allow a paid API after a subscription</label>
          <p v-if="staged.tiers[item.tier]!.metered_after_subscription" class="route-warning">Warning: when the subscription is full or unavailable, this tier can fall back to paid API calls billed against the monthly budget.</p>
          <p v-else-if="paidAfterSubscription(staged.tiers[item.tier]!.chain)" class="route-warning">A paid API follows a subscription. Move it up, remove it, or allow it explicitly.</p>
          <div class="action-row">
            <button class="small secondary" :disabled="locked" :aria-label="`Use built-in order for ${item.label}`" @click="confirm('reset', item.tier)">Use built-in order…</button>
          </div>
        </template>
      </div>
    </div>
    <div v-if="list.length || progress" class="change-bar" role="region" aria-label="Unsaved routing changes">
      <span role="status"><strong>{{ progress || `${list.length} unsaved ${list.length === 1 ? "change" : "changes"}` }}</strong><template v-if="!progress"> · applying restarts Jarvis Core</template></span>
      <span v-if="problems.length && !progress" class="route-warning">{{ problems[0] }}</span>
      <span class="action-row">
        <button class="small secondary" :disabled="busy" @click="discard">Discard</button>
        <button class="small" :disabled="busy || !!problems.length" @click="confirm('apply')">Apply…</button>
      </span>
    </div>
  </section>
  <ConfirmDialog v-if="pending" :title="pending.spec.title" :detail="pending.spec.detail" :confirm-label="pending.spec.label" :tone="pending.spec.tone" @cancel="pending = null" @confirm="confirmed" />
</template>

<style scoped>
.change-bar {
  position: sticky; bottom: 16px; z-index: 5; display: flex; flex-wrap: wrap; align-items: center; justify-content: space-between; gap: 8px 14px;
  margin-top: 16px; padding: 10px 10px 10px 18px; border-radius: var(--r-lg); background: var(--bg-raised); box-shadow: var(--shadow-pop); font-size: 13px;
}
.change-bar strong { font-weight: 600; }
.change-bar .route-warning { margin: 0; }
</style>
