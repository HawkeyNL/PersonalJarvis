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
  <PageHeader title="System" description="Non-secret Home Node and build provenance. Hardware identifiers and process environment are intentionally omitted." :busy="busy"><button class="secondary" @click="load">Refresh</button></PageHeader>
  <ErrorPanel v-if="error" title="Could not load system information" :message="error" />
  <div v-if="!data && !error" class="empty-state card" role="status"><span><span class="spinner" aria-hidden="true"></span> Loading system information…</span></div>
  <section v-if="data" class="card tight" aria-labelledby="system-title">
    <div class="card-title"><h2 id="system-title">Home Node</h2><span class="t-xs t-2">Read-only</span></div>
    <div class="table-scroll">
      <table class="tbl system-table">
        <tbody><tr v-for="[key, value] in data.values" :key="key"><th scope="row">{{ key }}</th><td class="mono wrap-anywhere">{{ value }}</td></tr></tbody>
      </table>
    </div>
    <div v-if="!data.values.length" class="empty-state">No system information reported.</div>
  </section>
</template>
<style scoped>
.system-table th { width: 32%; }
.system-table th::first-letter { text-transform: uppercase; } /* data keys such as "release" */
</style>
