<script setup lang="ts">
import { onMounted, ref } from "vue";
import { api, errorText, type ServiceRecord } from "../admin";
import ErrorPanel from "../components/ErrorPanel.vue";
import PageHeader from "../components/PageHeader.vue";
import StatusBadge from "../components/StatusBadge.vue";
const rows = ref<ServiceRecord[]>([]); const busy = ref(false); const error = ref("");
async function load() { busy.value = true; error.value = ""; try { rows.value = await api.services(); } catch (e) { error.value = errorText(e); } finally { busy.value = false; } }
onMounted(load);
</script>
<template>
  <PageHeader title="Services" description="Visibility for the fixed Jarvis-owned service allowlist. Arbitrary units are never accepted." :busy="busy"><button class="secondary" @click="load">Refresh</button></PageHeader>
  <ErrorPanel v-if="error" :message="error" />
  <section class="table-card"><table><thead><tr><th>Jarvis service</th><th>State</th></tr></thead><tbody><tr v-for="row in rows" :key="row.name"><td>{{ row.name }}</td><td><StatusBadge :state="row.state" /></td></tr></tbody></table></section>
</template>
