<script setup lang="ts">
import { computed, inject, nextTick, onBeforeUnmount, onMounted, ref, watch } from "vue";
import { api, errorText, type LogRecord } from "../admin";
import ErrorPanel from "../components/ErrorPanel.vue";
import PageHeader from "../components/PageHeader.vue";
import { FOLLOW_IDLE_MS, FOLLOW_INTERVAL_MS, LOG_SERVICES, createFollow, pickService } from "../components/logs/logs";

// `service` preselects a unit ("Open logs" on Overview); only allowlisted values count.
const props = defineProps<{ service?: string }>();
const selected = ref(pickService(props.service));
const level = ref("ALL"); const search = ref(""); const records = ref<LogRecord[]>([]); const unit = ref(""); const busy = ref(false); const error = ref("");
const follow = ref(false);
const pausedBy = ref<"" | "idle" | "error">("");
const viewport = ref<HTMLElement | null>(null);
// True while the newest line is in view: new lines then keep it in view.
const atLatest = ref(true);
const pollingBlocked = inject("pollingBlocked", () => false);
let mounted = false;
const filtered = computed(() => { const needle = search.value.toLowerCase(); return records.value.filter((record) => (level.value === "ALL" || record.level === level.value) && (!needle || `${record.timestamp ?? ""} ${record.level} ${record.target ?? ""} ${record.source_ip ?? ""} ${record.message} ${record.details.flat().join(" ")}`.toLowerCase().includes(needle))); });
// The quiet status line changes only with the Follow state, never per poll.
const status = computed(() => {
  if (follow.value) return `Following: new lines every ${FOLLOW_INTERVAL_MS / 1000} seconds. Pauses after ${FOLLOW_IDLE_MS / 1000} seconds without activity.`;
  if (pausedBy.value === "idle") return `Paused after ${FOLLOW_IDLE_MS / 1000} seconds without activity. Press Follow to continue.`;
  if (pausedBy.value === "error") return "Paused: the log could not be read.";
  return "Follow is off. The list holds still while you read.";
});

function onScroll() {
  const el = viewport.value;
  if (el) atLatest.value = el.scrollHeight - el.scrollTop - el.clientHeight < 24;
}
function toLatest() {
  const el = viewport.value;
  if (el) el.scrollTop = el.scrollHeight;
  atLatest.value = true;
}
function jumpToLatest() {
  toLatest();
  // The button goes away once the latest line is in view; focus stays in the list.
  viewport.value?.focus({ preventScroll: true });
}
async function load(quiet = false) {
  const service = selected.value;
  if (!quiet) busy.value = true;
  try {
    const response = await api.logs(service, 750);
    // A late answer for another service never replaces the current list.
    if (service !== selected.value) return;
    const stick = atLatest.value;
    error.value = "";
    records.value = response.records; unit.value = response.unit;
    if (stick) await nextTick(toLatest);
  } catch (e) {
    if (service !== selected.value) return;
    error.value = errorText(e);
    if (follow.value) { follow.value = false; pausedBy.value = "error"; }
  } finally { if (service === selected.value) busy.value = false; }
}
const follower = createFollow(() => load(true), () => mounted && !pollingBlocked(), () => { follow.value = false; pausedBy.value = "idle"; });
function toggleFollow() {
  follow.value = !follow.value;
  pausedBy.value = "";
}
watch(follow, (on) => (on ? follower.start() : follower.stop()));
watch(selected, () => { atLatest.value = true; void load(); });
onMounted(() => {
  mounted = true;
  // Only the shell's events: they are the ones that count as activity.
  const shell = document.querySelector(".app-shell");
  if (shell) follower.listen(shell);
  void load();
});
onBeforeUnmount(() => { mounted = false; follower.dispose(); });
</script>
<template>
  <div class="logs-page">
    <PageHeader title="Logs" description="Bounded, sanitized and parsed output from the fixed Jarvis journal allowlist." :busy="busy">
      <button class="secondary" @click="() => load()">Refresh</button>
      <button type="button" :class="follow ? 'active-button' : 'secondary'" :aria-pressed="follow" @click="toggleFollow">
        <svg v-if="follow" width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" aria-hidden="true"><path d="M9 5v14M15 5v14" /></svg>
        <svg v-else width="16" height="16" viewBox="0 0 24 24" fill="currentColor" aria-hidden="true"><path d="M7 4.5v15l12-7.5z" /></svg>
        Follow
      </button>
    </PageHeader>
    <ErrorPanel v-if="error" title="Could not load logs" :message="error" />
    <section class="log-toolbar">
      <label>Service<select v-model="selected"><option v-for="item in LOG_SERVICES" :key="item.value" :value="item.value">{{ item.label }}</option></select></label>
      <label>Level<select v-model="level"><option v-for="item in ['ALL','ERROR','WARN','INFO','DEBUG','SYSTEM']" :key="item">{{ item }}</option></select></label>
      <label class="log-search">Search<input v-model="search" type="search" placeholder="Search visible logs…" /></label>
      <span class="log-count">{{ filtered.length }} {{ filtered.length === 1 ? "line" : "lines" }} · {{ unit }}</span>
    </section>
    <p class="log-status" role="status">{{ status }}</p>
    <div class="log-frame">
      <section ref="viewport" class="log-viewport" tabindex="0" aria-label="Log lines, newest last" @scroll="onScroll">
        <article v-for="record in filtered" :key="`${record.id}-${record.timestamp}`" class="log-entry" :class="`level-${record.level.toLowerCase()}`">
          <div class="log-line"><time>{{ record.timestamp ?? "--:--:--" }}</time><span class="log-level">{{ record.level }}</span><span v-if="record.target" class="log-target">{{ record.target }}</span><span class="log-message"><span v-if="record.source_ip" class="log-ip">[{{ record.source_ip }}] </span>{{ record.message }}</span></div>
          <details v-if="record.details.length"><summary>Structured details</summary><dl><template v-for="[key, value] in record.details" :key="key"><dt>{{ key }}</dt><dd>{{ value }}</dd></template></dl></details>
        </article>
        <div v-if="!filtered.length && !busy" class="empty-state">No matching safe log records.</div>
      </section>
      <button v-if="!atLatest && filtered.length" type="button" class="small primary log-latest" @click="jumpToLatest">Jump to latest</button>
    </div>
  </div>
</template>
