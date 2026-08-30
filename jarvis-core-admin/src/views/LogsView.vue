<script setup lang="ts">
import { computed, nextTick, onBeforeUnmount, onMounted, ref, watch } from "vue";
import { api, errorText, type LogRecord, type LogService } from "../admin";
import ErrorPanel from "../components/ErrorPanel.vue";
import PageHeader from "../components/PageHeader.vue";

const services: { value: LogService; label: string }[] = [
  { value: "core", label: "Core" }, { value: "surrealdb", label: "SurrealDB" },
  { value: "config-broker", label: "Config broker" }, { value: "codex-broker", label: "Codex broker" },
  { value: "opensandbox", label: "OpenSandbox" }, { value: "updater", label: "Updater" },
  { value: "agents-updater", label: "Agent updater" },
];
const service = ref<LogService>("core"); const level = ref("ALL"); const search = ref(""); const records = ref<LogRecord[]>([]); const unit = ref(""); const busy = ref(false); const error = ref(""); const follow = ref(false); const viewport = ref<HTMLElement | null>(null); let timer: number | undefined;
const filtered = computed(() => { const needle = search.value.toLowerCase(); return records.value.filter((record) => (level.value === "ALL" || record.level === level.value) && (!needle || `${record.timestamp ?? ""} ${record.level} ${record.target ?? ""} ${record.message} ${record.details.flat().join(" ")}`.toLowerCase().includes(needle))); });
async function load(quiet = false) { if (!quiet) busy.value = true; error.value = ""; try { const response = await api.logs(service.value, 750); records.value = response.records; unit.value = response.unit; if (follow.value) await nextTick(() => { if (viewport.value) viewport.value.scrollTop = viewport.value.scrollHeight; }); } catch (e) { error.value = errorText(e); follow.value = false; } finally { busy.value = false; } }
function syncFollow() { clearInterval(timer); if (follow.value) { load(); timer = window.setInterval(() => load(true), 2500); } }
watch(service, () => load()); watch(follow, syncFollow); onMounted(load); onBeforeUnmount(() => clearInterval(timer));
</script>
<template>
  <div class="logs-page">
    <PageHeader title="Logs" description="Bounded, sanitized and parsed output from the fixed Jarvis journal allowlist." :busy="busy"><button class="secondary" @click="() => load()">Refresh</button><button :class="follow ? 'active-button' : 'secondary'" @click="follow = !follow">{{ follow ? "Following" : "Follow" }}</button></PageHeader>
    <ErrorPanel v-if="error" :message="error" />
    <section class="log-toolbar">
      <label>Service<select v-model="service"><option v-for="item in services" :key="item.value" :value="item.value">{{ item.label }}</option></select></label>
      <label>Level<select v-model="level"><option v-for="item in ['ALL','ERROR','WARN','INFO','DEBUG','SYSTEM']" :key="item">{{ item }}</option></select></label>
      <label class="log-search">Search<input v-model="search" type="search" placeholder="Search visible logs…" /></label>
      <span class="log-count">{{ filtered.length }} · {{ unit }}</span>
    </section>
    <section ref="viewport" class="log-viewport" aria-live="polite">
      <article v-for="record in filtered" :key="`${record.id}-${record.timestamp}`" class="log-entry" :class="`level-${record.level.toLowerCase()}`">
        <div class="log-line"><time>{{ record.timestamp ?? "--:--:--" }}</time><span class="log-level">{{ record.level }}</span><span v-if="record.target" class="log-target">{{ record.target }}</span><span class="log-message">{{ record.message }}</span></div>
        <details v-if="record.details.length"><summary>Structured details</summary><dl><template v-for="[key, value] in record.details" :key="key"><dt>{{ key }}</dt><dd>{{ value }}</dd></template></dl></details>
      </article>
      <div v-if="!filtered.length && !busy" class="empty-state">No matching safe log records.</div>
    </section>
  </div>
</template>
