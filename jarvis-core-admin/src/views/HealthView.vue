<script setup lang="ts">
import { onMounted, ref } from "vue";
import { api, errorText, type HealthResponse } from "../admin";
import ErrorPanel from "../components/ErrorPanel.vue";
import PageHeader from "../components/PageHeader.vue";
import StatusBadge from "../components/StatusBadge.vue";
const data = ref<HealthResponse | null>(null); const busy = ref(false); const error = ref("");
async function load(verify = false) { busy.value = true; error.value = ""; try { data.value = await api.health(verify); } catch (e) { error.value = errorText(e); } finally { busy.value = false; } }
onMounted(() => load(false));
</script>
<template>
  <PageHeader title="Health" description="Local service checks and the trusted full deployment verifier." :busy="busy">
    <button class="secondary" @click="load(false)">Refresh</button><button @click="load(true)">Run full verification</button>
  </PageHeader>
  <ErrorPanel v-if="error" :message="error" />
  <section v-if="data" class="card-list">
    <article v-for="(state, name) in data.checks" :key="name" class="row-card"><strong>{{ name }}</strong><StatusBadge :state="state" /></article>
    <article class="detail-card"><span class="card-label">DEPLOYMENT VERIFIER</span><StatusBadge :state="data.verification ?? 'not run this session'" /></article>
  </section>
</template>
