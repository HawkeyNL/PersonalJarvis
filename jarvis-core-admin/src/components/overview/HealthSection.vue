<script setup lang="ts">
import { computed, ref } from "vue";
import { api, errorText } from "../../admin";
import { serviceSummary } from "../../stats";
import { stateLabel, toneOf, type Tone } from "../../status";
import StatusBadge from "../StatusBadge.vue";
import { logServiceFor } from "./needs";

// Health verdict, the service allowlist and the trusted full deployment check,
// all from the status Overview already read. The last full-check result lives
// only in this component: a lock unmounts the view and drops it.
const props = defineProps<{ services: Record<string, string> | null; updater: string | null; error: string }>();
const emit = defineEmits<{ logs: [] }>();

const summary = computed(() => serviceSummary(props.services));
const rows = computed(() => Object.entries(props.services ?? {}));
const verdict = computed<{ tone: Tone; label: string; text: string }>(() => {
  const { up, total, tone } = summary.value;
  if (!total) return { tone: "idle", label: "Unknown", text: "No Jarvis services reported." };
  if (up === total) return { tone, label: "Healthy", text: `All ${total} Jarvis services are active.` };
  return { tone, label: "Degraded", text: `${total - up} of ${total} Jarvis services are not active.` };
});
// The timer off is the owner's choice for manual installs, so it stays neutral.
const UPDATER: Record<string, string> = { disabled: "Manual · timer off", enabled: "Automatic · timer on" };
const updater = computed(() => {
  const value = props.updater ?? "";
  return UPDATER[value] ? { label: UPDATER[value], tone: "idle" as Tone } : { label: stateLabel(value), tone: toneOf(value) };
});

const checking = ref(false);
const lastCheck = ref<{ state: string; at: string; error: string } | null>(null);
const clock = () => new Date().toLocaleTimeString([], { hour: "2-digit", minute: "2-digit", second: "2-digit", hour12: false });
async function fullCheck() {
  if (checking.value) return;
  checking.value = true;
  try {
    const result = await api.health(true);
    lastCheck.value = { state: result.verification ?? "failed", at: clock(), error: "" };
  } catch (e) {
    lastCheck.value = { state: "", at: clock(), error: errorText(e) };
  } finally {
    checking.value = false;
  }
}
</script>

<template>
  <section class="health" aria-labelledby="health-title">
    <h2 id="health-title">Health</h2>
    <p v-if="services" class="verdict"><StatusBadge :tone="verdict.tone" :label="verdict.label" />{{ verdict.text }}</p>
    <p v-else-if="error" class="verdict"><StatusBadge tone="error" label="Unknown" />Could not read the services: {{ error }}</p>
    <p v-else class="verdict skeleton" aria-busy="true"><span class="sr-only">Reading service state…</span><i aria-hidden="true" /></p>

    <div class="table-card">
      <table>
        <thead><tr><th scope="col">Jarvis service</th><th scope="col">State</th><th scope="col"><span class="sr-only">Logs</span></th></tr></thead>
        <tbody v-if="services">
          <tr v-for="[name, state] in rows" :key="name">
            <td>{{ name }}</td>
            <td><StatusBadge :state="state" /></td>
            <td class="num"><button v-if="state.toLowerCase() !== 'active' && logServiceFor(name)" type="button" class="small secondary" :aria-label="`Open logs, then choose ${name}`" @click="emit('logs')">Open logs</button></td>
          </tr>
          <tr><td>Updater</td><td><StatusBadge :tone="updater.tone" :label="updater.label" /></td><td></td></tr>
        </tbody>
        <tbody v-else-if="!error" aria-hidden="true">
          <tr v-for="n in 3" :key="n" class="skeleton"><td><i /></td><td><i /></td><td></td></tr>
        </tbody>
      </table>
    </div>

    <div class="full-check">
      <button type="button" class="secondary" :disabled="checking" :aria-busy="checking || undefined" @click="fullCheck">Run full check (up to 3 min)</button>
      <p role="status">
        <template v-if="checking">Running the deployment verifier…</template>
        <template v-else-if="lastCheck?.error">Full check could not run at {{ lastCheck.at }}: {{ lastCheck.error }}</template>
        <template v-else-if="lastCheck"><StatusBadge :state="lastCheck.state" /> Last full check at {{ lastCheck.at }}</template>
        <template v-else>Full check not run in this session.</template>
      </p>
    </div>
  </section>
</template>

<style scoped>
.health { margin: 0 0 28px; }
.health h2 { margin: 0 0 12px; font-size: 15px; }
.verdict { display: flex; align-items: center; gap: 12px; margin: 0 0 12px; font-size: 14px; overflow-wrap: anywhere; }
.full-check { display: flex; flex-wrap: wrap; align-items: center; gap: 12px 16px; margin-top: 12px; }
.full-check p { display: flex; align-items: center; gap: 8px; margin: 0; color: var(--text-secondary); font-size: 13px; overflow-wrap: anywhere; }
.skeleton i { display: block; width: 96px; height: 12px; border-radius: var(--r-pill); background: var(--bg-sunken); }
.verdict.skeleton i { width: min(280px, 60%); }
</style>
