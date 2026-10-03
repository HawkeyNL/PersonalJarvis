<script setup lang="ts">
import { computed, onMounted, reactive, ref } from "vue";
import { api, errorText, type ModelRecord, type OperationResult, type RouteEntry, type RouteMutation, type RouteTier, type RoutingReport } from "../admin";
import ConfirmDialog from "./ConfirmDialog.vue";
import ErrorPanel from "./ErrorPanel.vue";
import ResultPanel from "./ResultPanel.vue";

const props = defineProps<{ models: ModelRecord[] }>();
const TIERS: { tier: RouteTier; label: string }[] = [
  { tier: "cheap", label: "Cheap" },
  { tier: "default", label: "Default" },
  { tier: "hard", label: "Hard" },
];
// jarvis_llm::ROUTING_PROVIDERS. Only claude-cli (subscription) and ollama (local) are not metered.
const ROUTE_PROVIDERS = ["anthropic-api", "openai-api", "deepseek-api", "xai-api", "zai-api", "ollama", "ollama-cloud", "huggingface", "claude-cli"];
const MAX_CHAIN = 9;
const REASONS: Record<string, string> = {
  routing_invalid: "routing.json is invalid.",
  routing_unsafe: "routing.json has unsafe ownership or permissions, or is a link.",
  routing_unreadable: "routing.json could not be read.",
  routing_too_large: "routing.json is larger than 64 KiB.",
};

interface Draft { chain: RouteEntry[]; metered: boolean; pick: string }
const report = ref<RoutingReport | null>(null);
const drafts = reactive<Record<RouteTier, Draft>>({ cheap: blank(), default: blank(), hard: blank() });
const busy = ref(false);
const error = ref("");
const result = ref<OperationResult | null>(null);
const confirmPaid = ref(false);

const candidates = computed(() => props.models.filter((row) => ROUTE_PROVIDERS.includes(row.provider)));
const paidApi = computed(() => report.value?.routing?.paid_api ?? "allowed");
const unavailable = computed(() => report.value?.routing_unavailable_reason ?? null);
// The CLI refuses to edit an unusable file; it has to be repaired as root first.
const locked = computed(() => busy.value || !report.value || !!unavailable.value);

function blank(): Draft { return { chain: [], metered: false, pick: "" }; }
const key = (entry: RouteEntry) => `${entry.provider}/${entry.model}`;
const kind = (provider: string) => provider === "claude-cli" ? "subscription" : provider === "ollama" ? "local" : "paid API";
const stored = (tier: RouteTier) => report.value?.routing?.tiers[tier] ?? null;
const enabled = (entry: RouteEntry) => props.models.some((row) => key(row) === key(entry) && row.enabled);
function paidAfterSubscription(chain: RouteEntry[]): boolean {
  const first = chain.findIndex((entry) => entry.provider === "claude-cli");
  return first >= 0 && chain.slice(first + 1).some((entry) => kind(entry.provider) === "paid API");
}
function dirty(tier: RouteTier): boolean {
  const saved = stored(tier);
  return JSON.stringify([drafts[tier].chain, drafts[tier].metered]) !== JSON.stringify([saved?.chain ?? [], saved?.metered_after_subscription ?? false]);
}
function canSave(tier: RouteTier): boolean {
  const draft = drafts[tier];
  return !locked.value && dirty(tier) && draft.chain.length > 0 && (draft.metered || !paidAfterSubscription(draft.chain));
}
function add(tier: RouteTier) {
  const draft = drafts[tier];
  const row = candidates.value.find((item) => key(item) === draft.pick);
  if (row && draft.chain.length < MAX_CHAIN && !draft.chain.some((entry) => key(entry) === draft.pick)) draft.chain.push({ provider: row.provider, model: row.model });
  draft.pick = "";
}
function move(tier: RouteTier, index: number, delta: number) {
  const chain = drafts[tier].chain;
  [chain[index], chain[index + delta]] = [chain[index + delta], chain[index]];
}
async function load() {
  busy.value = true; error.value = "";
  try {
    report.value = await api.modelRoutes();
    for (const { tier } of TIERS) {
      const saved = stored(tier);
      drafts[tier] = { chain: saved ? saved.chain.map((entry) => ({ ...entry })) : [], metered: saved?.metered_after_subscription ?? false, pick: "" };
    }
  } catch (reason) { error.value = errorText(reason); }
  finally { busy.value = false; }
}
async function apply(request: RouteMutation) {
  busy.value = true; error.value = ""; result.value = null; confirmPaid.value = false;
  try { result.value = await api.modelRouteMutation(request); }
  catch (reason) { error.value = errorText(reason); }
  finally { busy.value = false; }
  await load();
}
onMounted(() => { void load(); });
</script>

<template>
  <section class="detail-card routing-card">
    <div class="card-heading">
      <div><span class="card-label">MODEL ROUTING</span><h2>Order per tier</h2></div>
      <div class="paid-switch" :class="paidApi">
        <span>Paid APIs: <strong>{{ paidApi }}</strong></span>
        <button v-if="paidApi === 'allowed'" class="small secondary" :disabled="locked" @click="apply({ action: 'paid_api', state: 'off' })">Turn off</button>
        <button v-else class="small secondary" :disabled="locked" @click="confirmPaid = true">Allow</button>
      </div>
    </div>
    <p class="usage-source">Paid APIs off keeps only subscriptions (claude-cli) and local Ollama in every tier. Routing only orders models: a model still has to be enabled, and disabled models are skipped. Every change restarts Jarvis Core.</p>
    <ErrorPanel v-if="unavailable" :message="`${REASONS[unavailable] ?? 'Routing is unavailable.'} Jarvis uses the built-in order without paid APIs until it is fixed as root (${unavailable}).`" />
    <ErrorPanel v-if="error" :message="error" /><ResultPanel v-if="result" :result="result" />
    <div v-if="report" class="routing-grid">
      <div v-for="item in TIERS" :key="item.tier" class="routing-tier">
        <span class="card-label">{{ item.label }} · {{ stored(item.tier) ? "owner order" : "built-in order" }}</span>
        <ol v-if="drafts[item.tier].chain.length" class="route-chain">
          <li v-for="(entry, index) in drafts[item.tier].chain" :key="key(entry)">
            <span class="mono wrap-anywhere">{{ entry.provider }} / {{ entry.model }}</span>
            <small>{{ kind(entry.provider) }}{{ enabled(entry) ? "" : " · not enabled, skipped" }}</small>
            <span class="action-row">
              <button class="small secondary" :disabled="locked || index === 0" aria-label="Move up" @click="move(item.tier, index, -1)">↑</button>
              <button class="small secondary" :disabled="locked || index === drafts[item.tier].chain.length - 1" aria-label="Move down" @click="move(item.tier, index, 1)">↓</button>
              <button class="small danger ghost" :disabled="locked" aria-label="Remove" @click="drafts[item.tier].chain.splice(index, 1)">✕</button>
            </span>
          </li>
        </ol>
        <p v-else class="usage-source">No owner order: Jarvis uses its built-in order for this tier.</p>
        <div class="route-controls">
          <select v-model="drafts[item.tier].pick" :disabled="locked || drafts[item.tier].chain.length >= MAX_CHAIN" :aria-label="`Add a model to ${item.label}`">
            <option value="">{{ drafts[item.tier].chain.length >= MAX_CHAIN ? `At most ${MAX_CHAIN} models` : "Add a discovered model…" }}</option>
            <option v-for="row in candidates" :key="key(row)" :value="key(row)" :disabled="drafts[item.tier].chain.some((entry) => key(entry) === key(row))">{{ row.provider }} / {{ row.model }}{{ row.enabled ? "" : " (disabled)" }}</option>
          </select>
          <button class="small secondary" :disabled="locked || !drafts[item.tier].pick" @click="add(item.tier)">Add</button>
        </div>
        <label class="route-check"><input v-model="drafts[item.tier].metered" type="checkbox" :disabled="locked" />Allow a paid API after a subscription</label>
        <p v-if="drafts[item.tier].metered" class="route-warning">Warning: when the subscription is full or unavailable, this tier can fall back to paid API calls billed against the monthly budget.</p>
        <p v-else-if="paidAfterSubscription(drafts[item.tier].chain)" class="route-warning">A paid API follows a subscription. Move it up, remove it, or allow it explicitly.</p>
        <div class="action-row">
          <button class="small" :disabled="!canSave(item.tier)" @click="apply({ action: 'set', tier: item.tier, chain: drafts[item.tier].chain, metered_after_subscription: drafts[item.tier].metered })">Save order</button>
          <button class="small secondary" :disabled="locked || !stored(item.tier)" @click="apply({ action: 'reset', tier: item.tier })">Use built-in order</button>
        </div>
      </div>
    </div>
  </section>
  <ConfirmDialog v-if="confirmPaid" title="Allow paid APIs?" detail="Metered provider APIs become eligible again in every tier, within the monthly budget. Providers bill each call." confirm-label="Allow paid APIs" @cancel="confirmPaid = false" @confirm="apply({ action: 'paid_api', state: 'allowed' })" />
</template>
