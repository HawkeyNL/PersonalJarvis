<script setup lang="ts">
import { onMounted, ref } from "vue";
import { api, errorText, type OverviewResponse } from "../admin";
import ErrorPanel from "../components/ErrorPanel.vue";
import PageHeader from "../components/PageHeader.vue";
import StatusBadge from "../components/StatusBadge.vue";

const data = ref<OverviewResponse | null>(null);
const busy = ref(false);
const error = ref("");
async function load() {
  busy.value = true; error.value = "";
  try { data.value = await api.overview(); } catch (e) { error.value = errorText(e); }
  finally { busy.value = false; }
}
onMounted(load);
</script>

<template>
  <PageHeader title="Overview" description="Current local Home Node state. Remote update checks run only when requested." :busy="busy">
    <button class="secondary" @click="load">Refresh</button>
  </PageHeader>
  <ErrorPanel v-if="error" :message="error" />
  <template v-if="data">
    <section class="hero-card">
      <div><span class="card-label">ACTIVE RELEASE</span><strong>{{ data.status.release ?? "Unavailable" }}</strong></div>
      <StatusBadge :state="data.update?.update ?? 'update unknown'" />
    </section>
    <section class="metric-grid">
      <article v-for="(state, name) in data.status.services" :key="name" class="metric-card">
        <span class="card-label">{{ name }}</span><StatusBadge :state="state" />
      </article>
      <article class="metric-card"><span class="card-label">UPDATER</span><StatusBadge :state="data.status.updater_enabled" /></article>
      <article class="metric-card"><span class="card-label">AGENT BUNDLE</span><strong>{{ data.status.agent_bundle?.id ?? "Unavailable" }}</strong><small>{{ data.status.agent_bundle?.agent_count ?? 0 }} agents</small></article>
    </section>
  </template>
  <div v-else-if="busy" class="empty-state">Authorizing and loading Home Node state…</div>
</template>
