<script setup lang="ts">
import { pairingError, pairingRequests, approvePairing, denyPairing } from "../pairingApprovals";
</script>
<template>
  <aside v-if="pairingRequests.length" class="pairing-approvals">
    <p v-for="request in pairingRequests" :key="request.id">
      Nieuw apparaat: {{ request.device_name }} ({{ request.platform }}, {{ request.fingerprint }})
      <button @click="approvePairing(request)">Approve</button>
      <button @click="denyPairing(request)">Deny</button>
    </p>
    <small v-if="pairingError">{{ pairingError }}</small>
  </aside>
</template>
