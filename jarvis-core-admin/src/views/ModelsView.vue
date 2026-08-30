<script setup lang="ts">
import { computed, onMounted, ref } from "vue";
import { api, errorText, type ModelRecord, type OperationResult } from "../admin";
import ConfirmDialog from "../components/ConfirmDialog.vue";
import ErrorPanel from "../components/ErrorPanel.vue";
import PageHeader from "../components/PageHeader.vue";
import ResultPanel from "../components/ResultPanel.vue";
import StatusBadge from "../components/StatusBadge.vue";
const rows = ref<ModelRecord[]>([]); const query = ref(""); const busy = ref(false); const error = ref(""); const result = ref<OperationResult | null>(null); const disableTarget = ref<ModelRecord | null>(null);
const filtered = computed(() => { const q = query.value.toLowerCase(); return rows.value.filter((row) => `${row.provider} ${row.model} ${row.source}`.toLowerCase().includes(q)); });
async function load() { busy.value = true; error.value = ""; try { rows.value = await api.models(); } catch (e) { error.value = errorText(e); } finally { busy.value = false; } }
async function mutate(action: "refresh" | "enable" | "disable", row?: ModelRecord) { busy.value = true; error.value = ""; disableTarget.value = null; try { const request: Record<string, string> = { action }; if (row) { request.provider = row.provider; request.model = row.model; } result.value = await api.modelMutation(request); await load(); } catch (e) { error.value = errorText(e); } finally { busy.value = false; } }
onMounted(load);
</script>
<template>
  <PageHeader title="Models" description="Typed model policy controls. Provider and model values never become shell input." :busy="busy"><button class="secondary" @click="load">Refresh view</button><button @click="mutate('refresh')">Refresh catalog</button></PageHeader>
  <ErrorPanel v-if="error" :message="error" /><ResultPanel v-if="result" :result="result" />
  <div class="toolbar"><input v-model="query" class="search" placeholder="Search provider, model or source…" /></div>
  <section class="table-card"><table><thead><tr><th>Provider</th><th>Model</th><th>State</th><th>Source</th><th></th></tr></thead><tbody><tr v-for="row in filtered" :key="`${row.provider}/${row.model}`"><td>{{ row.provider }}</td><td class="mono wrap-anywhere">{{ row.model }}</td><td><StatusBadge :state="row.enabled ? 'enabled' : 'disabled'" /></td><td>{{ row.source }}</td><td class="table-actions"><button v-if="!row.enabled" class="small secondary" @click="mutate('enable', row)">Enable</button><button v-else class="small danger ghost" @click="disableTarget = row">Disable</button></td></tr></tbody></table></section>
  <ConfirmDialog v-if="disableTarget" title="Disable this model?" :detail="`${disableTarget.provider}/${disableTarget.model} will no longer be eligible for routing.`" confirm-label="Disable model" @cancel="disableTarget = null" @confirm="mutate('disable', disableTarget ?? undefined)" />
</template>
