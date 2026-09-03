<script setup lang="ts">
import { onMounted, ref } from "vue";
import { api, errorText, type SystemResponse } from "../admin";
import ErrorPanel from "../components/ErrorPanel.vue";
import PageHeader from "../components/PageHeader.vue";
const data = ref<SystemResponse | null>(null); const busy = ref(false); const error = ref("");
async function load() { busy.value = true; error.value = ""; try { data.value = await api.system(); } catch (e) { error.value = errorText(e); } finally { busy.value = false; } }
onMounted(load);
</script>
<template>
  <PageHeader title="System / About" description="Non-secret Home Node and build provenance. Hardware identifiers and process environment are intentionally omitted." :busy="busy"><button class="secondary" @click="load">Refresh</button></PageHeader>
  <ErrorPanel v-if="error" :message="error" />
  <section v-if="data" class="system-grid"><article v-for="[key, value] in data.values" :key="key" class="detail-card system-card"><span class="card-label">{{ key }}</span><strong class="wrap-anywhere">{{ value }}</strong></article></section>
</template>
