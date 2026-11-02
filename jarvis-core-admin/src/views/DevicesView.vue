<script setup lang="ts">
import { computed, onBeforeUnmount, onMounted, ref, watch } from "vue";
import { api, errorText, type DeviceOverview, type DeviceAction, type LocalDevice, type PendingDevice } from "../admin";
import PageHeader from "../components/PageHeader.vue";
import ErrorPanel from "../components/ErrorPanel.vue";
import ConfirmDialog from "../components/ConfirmDialog.vue";
import StatusBadge from "../components/StatusBadge.vue";
import PairingReviewDialog from "../components/devices/PairingReviewDialog.vue";
import { REFRESH_TIMEOUT_MS, approveAction, countdown, createActivityRefresh, isExpired, requestedAt, withTimeout } from "../components/devices/pairing";
const ACTIVATION_COMMAND = "sudo jarvis account activation-code --allow-cidr <private-device-ip>/32";
const data = ref<DeviceOverview | null>(null);
const busy = ref(false);
const error = ref("");
const errorTitle = ref<string>();
const result = ref("");
const now = ref(Date.now());
const review = ref<PendingDevice | null>(null);
const revoking = ref<LocalDevice | null>(null);
const emit = defineEmits<{ pairings: [count: number] }>();
// The sidebar badge counts the requests that can still be approved.
const pendingCount = computed(() => data.value?.requests.filter((request) => !isExpired(request.expires_at, now.value)).length);
watch(pendingCount, (count) => { if (count !== undefined) emit("pairings", count); });
const time = (date: Date) => date.toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" });
let latest = 0;
// A late answer from an earlier load never overwrites a newer one.
async function fetchDevices() {
  const call = ++latest;
  const next = await withTimeout(api.devices(), REFRESH_TIMEOUT_MS);
  if (call === latest) data.value = next;
}
const LOAD_FAILED = "Could not load devices";
// Background refresh on activity (see pairing.ts): silent, keeps the current
// list on failure and never replaces an action's error.
const refresher = createActivityRefresh(async () => {
  const own = !error.value || errorTitle.value === LOAD_FAILED;
  try { await fetchDevices(); if (own) error.value = ""; }
  catch (e) { if (own) { error.value = errorText(e); errorTitle.value = LOAD_FAILED; } }
}, () => !busy.value && !review.value && !revoking.value);
async function load() {
  busy.value = true; error.value = "";
  refresher.refreshed();
  try { await fetchDevices(); }
  catch (e) { error.value = errorText(e); errorTitle.value = LOAD_FAILED; }
  finally { busy.value = false; }
}
async function apply(request: DeviceAction, done: string, failed: string) {
  if (busy.value) return;
  review.value = null; revoking.value = null;
  busy.value = true; error.value = ""; result.value = "";
  try {
    await api.deviceAction(request);
    result.value = done;
  } catch (e) {
    error.value = errorText(e); errorTitle.value = failed;
  }
  // Reload either way (an approved device moves to Trusted), keeping the
  // action's result or error; Refresh retries a failed reload.
  refresher.refreshed();
  try { await fetchDevices(); } catch { /* keep the action's message */ }
  finally { busy.value = false; }
}
function approve(request: PendingDevice) {
  return apply(approveAction(request), `${request.name} is approved and now a trusted device.`, `Could not approve ${request.name}`);
}
function deny(request: PendingDevice) {
  return apply({ action: "deny", request_id: request.id }, `The request from ${request.name} is denied.`, `Could not deny ${request.name}`);
}
function revoke(device: LocalDevice) {
  return apply({ action: "revoke", device_id: device.id }, `${device.name} is revoked.`, `Could not revoke ${device.name}`);
}
let clock: number | undefined;
onMounted(() => {
  void load();
  // Local clock for the countdowns only; it never calls the broker.
  clock = window.setInterval(() => (now.value = Date.now()), 1000);
  // Only the shell's events: they are the ones that count as activity.
  const shell = document.querySelector(".app-shell");
  if (shell) refresher.start(shell);
});
onBeforeUnmount(() => {
  clearInterval(clock);
  refresher.stop();
});
</script>
<template>
  <PageHeader title="Devices" description="Approve new devices and remove old ones. Every change asks for the administrator password in a system dialog." :busy="busy">
    <button class="secondary" :disabled="busy" @click="load">Refresh</button>
  </PageHeader>
  <ErrorPanel v-if="error" :message="error" :title="errorTitle" />
  <p v-if="result" class="device-result" role="status"><StatusBadge tone="ok" label="Done" />{{ result }}</p>
  <div v-if="!data" class="empty-state" role="status">
    <span v-if="busy"><span class="spinner" aria-hidden="true"></span> Loading devices…</span>
    <span v-else>Devices are not available.</span>
  </div>
  <template v-else>
    <section class="device-section" aria-labelledby="pending-title">
      <h2 id="pending-title">Waiting for approval</h2>
      <p class="section-hint">The list checks for new requests every 15 seconds while you use this page.</p>
      <p v-if="!data.requests.length" class="empty-state card">No devices are waiting. Start pairing in the Jarvis app on the new device; its request appears here.</p>
      <ul v-else class="request-list">
        <li v-for="request in data.requests" :key="request.id" class="card request-card" :class="{ expired: isExpired(request.expires_at, now) }">
          <div class="request-main">
            <strong>{{ request.name }}</strong>
            <span class="request-meta">{{ request.platform }} · requested {{ time(requestedAt(request.expires_at)) }}</span>
          </div>
          <StatusBadge v-if="isExpired(request.expires_at, now)" tone="idle" label="Expired" />
          <span v-else class="countdown">Expires in {{ countdown(request.expires_at, now) }}</span>
          <!-- aria-disabled on expiry: a focused button that expires keeps focus. -->
          <button class="btn primary" :disabled="busy" :aria-disabled="isExpired(request.expires_at, now)" :aria-label="`Review ${request.name}`" @click="isExpired(request.expires_at, now) || (review = request)">Review…</button>
        </li>
      </ul>
    </section>
    <section class="device-section" aria-labelledby="trusted-title">
      <h2 id="trusted-title">Trusted devices</h2>
      <div v-if="!data.devices.length" class="card first-device">
        <p><strong>No trusted devices.</strong></p>
        <p>First device: in a terminal on this Home Node, run</p>
        <code class="command">{{ ACTIVATION_COMMAND }}</code>
        <p>with the new device's local IP address, then enter the code it prints in the Jarvis app on that device within ten minutes.</p>
        <p class="section-hint">Was this Home Node set up before? First-device activation stays closed after revoking every device. Pair from the app with your account password instead and approve the request above.</p>
      </div>
      <div v-else class="table-card">
        <table class="tbl">
          <thead><tr><th>Device</th><th>Platform</th><th>Status</th><th class="table-actions"><span class="sr-only">Actions</span></th></tr></thead>
          <tbody>
            <tr v-for="device in data.devices" :key="device.id">
              <td>{{ device.name }}</td>
              <td>{{ device.platform }}</td>
              <td><StatusBadge :state="device.status" /></td>
              <td class="table-actions"><button class="danger ghost small" :disabled="busy" :aria-label="`Revoke ${device.name}`" @click="revoking = device">Revoke…</button></td>
            </tr>
          </tbody>
        </table>
      </div>
    </section>
  </template>
  <PairingReviewDialog v-if="review" :request="review" :now="now" @cancel="review = null" @approve="approve(review!)" @deny="deny(review!)" />
  <ConfirmDialog
    v-if="revoking"
    :title="`Revoke ${revoking.name}?`"
    :detail="`${revoking.name} (${revoking.platform}) loses access to this Home Node. Its sessions and live connections end right away. To use it again, pair it again and approve it here. Revoking every device does not reopen first-device activation. The system asks for the administrator password next.`"
    :confirm-label="`Revoke ${revoking.name}`"
    tone="danger"
    @cancel="revoking = null"
    @confirm="revoke(revoking!)"
  />
</template>
<style scoped>
.device-section { margin-top: 20px; }
.device-section h2 { margin: 0 0 4px; font-size: 15px; }
.section-hint { margin: 0 0 12px; color: var(--text-secondary); font-size: 13px; line-height: 1.5; }
.device-result { display: flex; align-items: center; gap: 8px; margin: 0 0 12px; }
.request-list { display: grid; gap: 8px; margin: 0; padding: 0; list-style: none; }
.request-card { display: flex; align-items: center; gap: 16px; padding: 14px 16px; }
.request-card.expired { color: var(--text-secondary); }
.request-main { display: flex; flex: 1; flex-direction: column; gap: 4px; min-width: 0; overflow-wrap: anywhere; }
.request-main strong { font-weight: 500; }
.request-meta { color: var(--text-secondary); font-size: 13px; }
.countdown { color: var(--text-secondary); font-size: 13px; font-variant-numeric: tabular-nums; white-space: nowrap; }
.first-device p { margin: 0 0 8px; font-size: 14px; line-height: 1.55; }
.command { display: block; margin: 0 0 8px; padding: 10px 12px; border-radius: var(--r-md); background: var(--bg-sunken); font: 13px/1.5 var(--font-mono); overflow-wrap: anywhere; user-select: all; }
</style>
