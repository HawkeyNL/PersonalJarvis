<script setup lang="ts">
import { computed, onBeforeUnmount, onMounted, ref, useId } from "vue";
import { api, errorText, type HfProvidersResponse, type ModelRecord, type OperationResult } from "../../admin";
import { dialogFocus } from "../../focus";
import ConfirmDialog from "../ConfirmDialog.vue";
import ErrorPanel from "../ErrorPanel.vue";
import ResultPanel from "../ResultPanel.vue";
import StatusBadge from "../StatusBadge.vue";
import { routeConfirm } from "./routing";

const props = defineProps<{ row: ModelRecord }>();
// `rows`: the catalog as reloaded after a saved route.
const emit = defineEmits<{ close: []; rows: [rows: ModelRecord[]] }>();
const id = useId();
const current = ref(props.row);
const details = ref<HfProvidersResponse | null>(null);
const selected = ref("auto");
const loading = ref(true);
const saving = ref(false);
const busy = computed(() => loading.value || saving.value);
const confirming = ref(false);
const confirmSpec = computed(() => routeConfirm(current.value, selected.value));
const error = ref("");
const errorTitle = ref<string>();
const result = ref<OperationResult | null>(null);
const dialog = ref<HTMLElement | null>(null);
let focus: ReturnType<typeof dialogFocus> | undefined;
let alive = true;

const price = (value: number | null) => value === null ? "—" : `$${value.toLocaleString(undefined, { minimumFractionDigits: 2, maximumFractionDigits: 4 })}`;
const yesNo = (value: boolean | null) => value === null ? "—" : value ? "yes" : "no";

function close() { if (!saving.value) emit("close"); }
async function save() {
  confirming.value = false;
  const row = current.value;
  saving.value = true; error.value = ""; result.value = null;
  try {
    // Saved (and Core restarted) even if a reload below fails: show it first.
    result.value = await api.modelMutation({ action: "set_route", provider: row.provider, model: row.model, route: selected.value });
    const rows = await api.models();
    if (!alive) return;
    emit("rows", rows);
    current.value = rows.find((item) => item.provider === row.provider && item.model === row.model) ?? row;
    details.value = await api.modelProviders(row.model);
    selected.value = current.value.route ?? "auto";
  } catch (reason) { error.value = errorText(reason); errorTitle.value = result.value ? "Route saved; could not reload routes" : undefined; }
  finally { saving.value = false; }
}
// Stays inside .app-shell (no Teleport), so the idle lock keeps seeing activity.
onMounted(async () => {
  focus = dialogFocus(dialog.value!, close);
  dialog.value!.querySelector<HTMLElement>("button")?.focus();
  try {
    const loaded = await api.modelProviders(props.row.model);
    if (alive) { details.value = loaded; selected.value = props.row.route ?? "auto"; }
  } catch (reason) { if (alive) { error.value = errorText(reason); errorTitle.value = "Could not load routes"; } }
  finally { loading.value = false; }
});
onBeforeUnmount(() => { alive = false; focus?.restore(); });
</script>

<template>
  <div class="dialog-backdrop" role="presentation" @click.self="close">
    <section ref="dialog" class="dialog routes-dialog" role="dialog" aria-modal="true" tabindex="-1" :aria-labelledby="`${id}-title`" :aria-busy="busy" @keydown="focus?.keydown($event)">
      <div class="card-heading"><div><span class="card-label">Hugging Face inference providers</span><h2 :id="`${id}-title`" class="mono wrap-anywhere">{{ current.model }}</h2></div><button class="small secondary" :disabled="saving" @click="close">Close</button></div>
      <ErrorPanel v-if="error" :message="error" :title="errorTitle" /><ResultPanel v-if="result" :result="result" />
      <div v-if="loading && !details" class="modal-loading" role="status"><span class="spinner" aria-hidden="true" />Loading available routes…</div>
      <template v-if="details">
        <div class="route-controls"><select v-model="selected" :disabled="busy" aria-label="Hugging Face route"><option v-for="route in details.routes" :key="route" :value="route">{{ route }}</option></select><button :disabled="busy" @click="confirming = true">{{ saving ? "Saving…" : "Save route…" }}</button></div>
        <p class="usage-source">Route selection does not enable this model. Dynamic routes use conservative budget estimates when the execution provider is not known.</p>
        <div class="table-card"><table><thead><tr><th>Route</th><th>Status</th><th>Context</th><th>Input/M</th><th>Output/M</th><th>TTFT</th><th>Tok/s</th><th>Tools</th><th>Structured</th></tr></thead><tbody>
          <tr v-for="item in details.providers" :key="item.provider"><td class="mono">{{ item.provider }}</td><td><StatusBadge :state="item.status" /></td><td class="mono">{{ item.context_length ?? "—" }}</td><td class="mono">{{ price(item.input_per_million_usd) }}</td><td class="mono">{{ price(item.output_per_million_usd) }}</td><td class="mono">{{ item.first_token_latency_ms === null ? "—" : `${item.first_token_latency_ms} ms` }}</td><td class="mono">{{ item.throughput ?? "—" }}</td><td>{{ yesNo(item.supports_tools) }}</td><td>{{ yesNo(item.supports_structured_output) }}</td></tr>
        </tbody></table></div>
      </template>
    </section>
  </div>
  <!-- A sibling, not a child: its Escape closes only the confirm. -->
  <ConfirmDialog v-if="confirming" :title="confirmSpec.title" :detail="confirmSpec.detail" :confirm-label="confirmSpec.label" :tone="confirmSpec.tone" @cancel="confirming = false" @confirm="save" />
</template>
