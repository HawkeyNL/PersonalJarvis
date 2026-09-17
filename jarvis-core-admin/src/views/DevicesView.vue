<script setup lang="ts">
import { onMounted, ref } from "vue";
import { api, errorText, type DeviceOverview, type DeviceAction } from "../admin";
import PageHeader from "../components/PageHeader.vue";
import ErrorPanel from "../components/ErrorPanel.vue";
import ConfirmDialog from "../components/ConfirmDialog.vue";
const data = ref<DeviceOverview | null>(null);
const busy = ref(false);
const error = ref("");
const result = ref("");
const selection = ref<{ request: DeviceAction; title: string; detail: string } | null>(null);
async function load() {
  busy.value = true; error.value = "";
  try { data.value = await api.devices(); }
  catch (e) { error.value = errorText(e); }
  finally { busy.value = false; }
}
async function apply() {
  const selected = selection.value;
  if (!selected || busy.value) return;
  selection.value = null; busy.value = true; error.value = ""; result.value = "";
  try {
    await api.deviceAction(selected.request);
    result.value = "Device access updated through local system authorization.";
    await load();
  } catch (e) { error.value = errorText(e); }
  finally { busy.value = false; }
}
onMounted(load);
</script>
<template>
  <PageHeader title="Devices" description="Home Node owner control. Every change requests the computer administrator password through the system dialog." :busy="busy">
    <button class="secondary" :disabled="busy" @click="load">Refresh</button>
  </PageHeader>
  <ErrorPanel v-if="error" :message="error" />
  <p v-if="result" role="status">{{ result }}</p>
  <section v-if="data" class="card-list">
    <h2>Waiting for approval</h2>
    <p v-if="!data.requests.length">No pending device requests.</p>
    <article v-for="request in data.requests" :key="request.id" class="detail-card">
      <strong>{{ request.name }} · {{ request.platform }}</strong>
      <p class="fingerprint">Fingerprint: {{ request.fingerprint }}</p>
      <p>Expires: {{ new Date(request.expires_at * 1000).toLocaleString() }}</p>
      <button :disabled="busy" @click="selection = { request: {action: 'approve', request_id: request.id, fingerprint: request.fingerprint}, title: `Allow ${request.name}?`, detail: `Confirm that this is your device. Fingerprint: ${request.fingerprint}` }">Allow…</button>
      <button class="secondary" :disabled="busy" @click="selection = { request: {action: 'deny', request_id: request.id}, title: `Deny ${request.name}?`, detail: 'This pending request will be rejected.' }">Deny…</button>
    </article>
    <h2>Trusted devices</h2>
    <p v-if="!data.devices.length">No active devices. First-device activation uses the local one-time code.</p>
    <article v-for="device in data.devices" :key="device.id" class="row-card">
      <strong>{{ device.name }} · {{ device.platform }}</strong>
      <button class="danger" :disabled="busy" @click="selection = { request: {action: 'revoke', device_id: device.id}, title: `Revoke ${device.name}?`, detail: 'This ends its sessions and live connections. Revoking every device does not reopen first-device activation.' }">Revoke…</button>
    </article>
  </section>
  <ConfirmDialog v-if="selection" :title="selection.title" :detail="selection.detail" confirm-label="Continue to system authorization" @cancel="selection = null" @confirm="apply" />
</template>
<style scoped>.fingerprint { overflow-wrap: anywhere; font-family: monospace; }</style>
