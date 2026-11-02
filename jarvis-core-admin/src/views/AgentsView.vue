<script setup lang="ts">
import { computed, onMounted, ref, useId } from "vue";
import { api, errorText, type AgentRecord, type AgentsResponse, type OperationResult } from "../admin";
import ConfirmDialog from "../components/ConfirmDialog.vue";
import ErrorPanel from "../components/ErrorPanel.vue";
import PageHeader from "../components/PageHeader.vue";
import ResultPanel from "../components/ResultPanel.vue";
import StatusBadge from "../components/StatusBadge.vue";
import { stateLabel } from "../status";
const data = ref<AgentsResponse | null>(null); const busy = ref(false); const error = ref(""); const errorTitle = ref<string>(); const expanded = ref(new Set<string>()); const selected = ref<AgentRecord | null>(null); const confirmUpdate = ref(false); const result = ref<OperationResult | null>(null);
const id = useId();
const groups = computed(() => { const map = new Map<string, AgentRecord[]>(); for (const agent of data.value?.agents ?? []) { const list = map.get(agent.group) ?? []; list.push(agent); map.set(agent.group, list); } return [...map.entries()].sort(([a], [b]) => a.localeCompare(b)); });
function toggle(group: string) { const next = new Set(expanded.value); next.has(group) ? next.delete(group) : next.add(group); expanded.value = next; }
function updatedLabel(value: string | null) { if (!value) return "Not recorded"; const date = new Date(value); return Number.isNaN(date.valueOf()) ? "Not recorded" : new Intl.DateTimeFormat(undefined, { dateStyle: "medium", timeStyle: "short" }).format(date); }
async function load() { busy.value = true; error.value = ""; try { data.value = await api.agents(); if (!expanded.value.size && groups.value[0]) expanded.value = new Set([groups.value[0][0]]); } catch (e) { error.value = errorText(e); errorTitle.value = "Could not load agents"; } finally { busy.value = false; } }
async function check() { busy.value = true; error.value = ""; try { result.value = await api.agentAction(false); await load(); } catch (e) { error.value = errorText(e); errorTitle.value = "Could not check for a bundle update"; } finally { busy.value = false; } }
async function update() { confirmUpdate.value = false; busy.value = true; error.value = ""; try { result.value = await api.agentAction(true); await load(); } catch (e) { error.value = errorText(e); errorTitle.value = "Could not update the agent bundle"; } finally { busy.value = false; } }
onMounted(load);
</script>
<template>
  <PageHeader title="Agents" description="Safe registry metadata only. Private prompts and agent definition bodies are never loaded by this application." :busy="busy">
    <button class="secondary" @click="load">Refresh</button>
    <button class="secondary" @click="check">Check for bundle update</button>
    <button @click="confirmUpdate = true">Update agent bundle…</button>
  </PageHeader>
  <ErrorPanel v-if="error" :message="error" :title="errorTitle" /><ResultPanel v-if="result" :result="result" />
  <section v-if="data" class="metric-grid agents-metrics">
    <article class="metric-card"><span class="card-label">Active bundle</span><strong class="mono">{{ data.bundle.id }}</strong></article>
    <article class="metric-card"><span class="card-label">Agents</span><strong>{{ data.bundle.agent_count }}</strong></article>
  </section>
  <!-- No data yet is loading, not "no agents": the empty tree text needs a loaded bundle. -->
  <div v-if="!data && !error" class="empty-state card" role="status"><span><span class="spinner" aria-hidden="true"></span> Loading agents…</span></div>
  <section v-else-if="data" class="split-view">
    <div class="tree-card">
      <div v-if="!groups.length" class="empty-state">Safe tree metadata is unavailable in this legacy bundle.</div>
      <div v-for="([group, agents], index) in groups" :key="group" class="tree-group">
        <button class="tree-group-button" :aria-expanded="expanded.has(group)" :aria-controls="`${id}-group-${index}`" @click="toggle(group)">
          <svg class="tree-chevron" width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><path d="m9 6 6 6-6 6" /></svg>
          <strong>{{ group }}</strong><small>{{ agents.length }}<span class="sr-only"> {{ agents.length === 1 ? "agent" : "agents" }}</span></small>
        </button>
        <div v-show="expanded.has(group)" :id="`${id}-group-${index}`" class="tree-children">
          <button v-for="agent in agents" :key="agent.id" class="tree-agent" :class="{ selected: selected?.id === agent.id }" :aria-pressed="selected?.id === agent.id" @click="selected = agent"><span>{{ agent.name }}</span><small v-if="agent.profile_lines">{{ agent.profile_lines }} lines</small><StatusBadge :state="agent.state" /></button>
        </div>
      </div>
    </div>
    <aside class="detail-card sticky-detail" aria-label="Agent details"><template v-if="selected"><span class="card-label">Safe agent details</span><h2>{{ selected.name }}</h2><dl><dt>ID</dt><dd>{{ selected.id }}</dd><dt>Group</dt><dd>{{ selected.group }}</dd><dt>State</dt><dd>{{ stateLabel(selected.state) }}</dd><dt>Model policy</dt><dd>{{ selected.model_policy ?? "Not declared" }}</dd><dt>Profile length</dt><dd>{{ selected.profile_lines ? `${selected.profile_lines} lines` : "Not recorded" }}</dd><dt>Source updated</dt><dd>{{ updatedLabel(selected.source_updated_at) }}</dd><dt>Bundle</dt><dd>{{ data.bundle.id }}</dd></dl><p class="security-note">Prompt/body: never loaded</p></template><div v-else class="empty-state">Select an agent to inspect safe metadata.</div></aside>
  </section>
  <ConfirmDialog
    v-if="confirmUpdate"
    title="Update the agent bundle?"
    :detail="`Replaces the active bundle${data ? ` ${data.bundle.id}` : ''} with the latest main of the private agent repository. The trusted validator checks the complete bundle first; if Core is not ready afterwards, the previous bundle is restored. This application never receives private prompt contents.`"
    confirm-label="Update agent bundle"
    tone="primary"
    @cancel="confirmUpdate = false"
    @confirm="update"
  />
</template>
<style scoped>
.agents-metrics { margin-bottom: 16px; }
.tree-chevron { flex: none; color: var(--text-secondary); }
.tree-group-button[aria-expanded="true"] .tree-chevron { rotate: 90deg; }
@media (prefers-reduced-motion: no-preference) { .tree-chevron { transition: rotate var(--d-hover) ease; } }
</style>
