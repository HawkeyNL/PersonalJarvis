<script setup lang="ts">
import { onMounted, ref } from "vue";
import { api, errorText, type CredentialRecord } from "../admin";
import ErrorPanel from "../components/ErrorPanel.vue";
import PageHeader from "../components/PageHeader.vue";
import StatusBadge from "../components/StatusBadge.vue";
const rows = ref<CredentialRecord[]>([]); const busy = ref(false); const error = ref("");
async function load() { busy.value = true; error.value = ""; try { rows.value = await api.credentials(); } catch (e) { error.value = errorText(e); } finally { busy.value = false; } }
onMounted(load);
</script>
<template>
  <PageHeader title="Credentials" description="Configuration status only. Secret values never enter Tauri IPC, frontend state, logs or process arguments." :busy="busy"><button class="secondary" @click="load">Refresh</button></PageHeader>
  <ErrorPanel v-if="error" :message="error" />
  <section class="credential-grid"><article v-for="row in rows" :key="row.provider" class="metric-card"><span class="card-label">{{ row.provider }}</span><StatusBadge :state="row.configured ? 'configured' : 'not configured'" /></article><article class="security-card"><strong>Protected secret entry</strong><p>Use <code>sudo jarvis credentials set …</code> in a trusted terminal. The GUI deliberately has no ordinary secret text field.</p></article></section>
</template>
