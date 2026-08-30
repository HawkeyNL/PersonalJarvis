<script setup lang="ts">
import { computed, onMounted, ref } from "vue";
import { api, errorText, type AgentRecord, type AgentsResponse, type OperationResult } from "../admin";
import ConfirmDialog from "../components/ConfirmDialog.vue";
import ErrorPanel from "../components/ErrorPanel.vue";
import PageHeader from "../components/PageHeader.vue";
import ResultPanel from "../components/ResultPanel.vue";
import StatusBadge from "../components/StatusBadge.vue";
const data = ref<AgentsResponse | null>(null); const busy = ref(false); const error = ref(""); const expanded = ref(new Set<string>()); const selected = ref<AgentRecord | null>(null); const confirmUpdate = ref(false); const result = ref<OperationResult | null>(null);
const groups = computed(() => { const map = new Map<string, AgentRecord[]>(); for (const agent of data.value?.agents ?? []) { const list = map.get(agent.group) ?? []; list.push(agent); map.set(agent.group, list); } return [...map.entries()].sort(([a], [b]) => a.localeCompare(b)); });
function toggle(group: string) { const next = new Set(expanded.value); next.has(group) ? next.delete(group) : next.add(group); expanded.value = next; }
function updatedLabel(value: string | null) { if (!value) return "Not recorded"; const date = new Date(value); return Number.isNaN(date.valueOf()) ? "Not recorded" : new Intl.DateTimeFormat(undefined, { dateStyle: "medium", timeStyle: "short" }).format(date); }
async function load() { busy.value = true; error.value = ""; try { data.value = await api.agents(); if (!expanded.value.size && groups.value[0]) expanded.value = new Set([groups.value[0][0]]); } catch (e) { error.value = errorText(e); } finally { busy.value = false; } }
async function check() { busy.value = true; error.value = ""; try { result.value = await api.agentAction(false); await load(); } catch (e) { error.value = errorText(e); } finally { busy.value = false; } }
async function update() { confirmUpdate.value = false; busy.value = true; error.value = ""; try { result.value = await api.agentAction(true); await load(); } catch (e) { error.value = errorText(e); } finally { busy.value = false; } }
onMounted(load);
</script>
<template>
  <PageHeader title="Agents" description="Safe registry metadata only. Private prompts and agent definition bodies are never loaded by this application." :busy="busy"><button class="secondary" @click="check">Check</button><button class="secondary" @click="load">Refresh</button><button @click="confirmUpdate = true">Update bundle</button></PageHeader>
  <ErrorPanel v-if="error" :message="error" /><ResultPanel v-if="result" :result="result" />
  <section v-if="data" class="hero-card"><div><span class="card-label">ACTIVE BUNDLE</span><strong>{{ data.bundle.id }}</strong></div><div><span class="card-label">AGENTS</span><strong>{{ data.bundle.agent_count }}</strong></div></section>
  <section class="split-view">
    <div class="tree-card">
      <div v-if="!groups.length" class="empty-state">Safe tree metadata is unavailable in this legacy bundle.</div>
      <div v-for="[group, agents] in groups" :key="group" class="tree-group">
        <button class="tree-group-button" @click="toggle(group)"><span>{{ expanded.has(group) ? "▼" : "▶" }}</span><strong>{{ group }}</strong><small>{{ agents.length }}</small></button>
        <div v-if="expanded.has(group)" class="tree-children">
          <button v-for="agent in agents" :key="agent.id" class="tree-agent" :class="{ selected: selected?.id === agent.id }" @click="selected = agent"><span>{{ agent.name }}</span><small v-if="agent.profile_lines">{{ agent.profile_lines }} lines</small><StatusBadge :state="agent.state" /></button>
        </div>
      </div>
    </div>
    <aside class="detail-card sticky-detail"><template v-if="selected"><span class="card-label">SAFE AGENT DETAILS</span><h2>{{ selected.name }}</h2><dl><dt>ID</dt><dd>{{ selected.id }}</dd><dt>Group</dt><dd>{{ selected.group }}</dd><dt>State</dt><dd>{{ selected.state }}</dd><dt>Model policy</dt><dd>{{ selected.model_policy ?? "Not declared" }}</dd><dt>Profile length</dt><dd>{{ selected.profile_lines ? `${selected.profile_lines} lines` : "Not recorded" }}</dd><dt>Source updated</dt><dd>{{ updatedLabel(selected.source_updated_at) }}</dd><dt>Bundle</dt><dd>{{ data?.bundle.id }}</dd></dl><p class="security-note">Prompt/body: never loaded</p></template><div v-else class="empty-state">Select an agent to inspect safe metadata.</div></aside>
  </section>
  <ConfirmDialog v-if="confirmUpdate" title="Update the active agent bundle?" detail="The trusted private-agent helper validates and activates the complete bundle. This GUI receives no private prompt contents." confirm-label="Confirm update" @cancel="confirmUpdate = false" @confirm="update" />
</template>
