<script setup lang="ts">
import { ref, onMounted } from "vue";
import { ibkrStatus, ibkrPositions, type IbkrPosition } from "../ibkr";

const status = ref<"checking" | "connected" | "disconnected">("checking");
const hint = ref<string | null>(null);
const account = ref<string | null>(null);
const positions = ref<IbkrPosition[]>([]);

async function refresh() {
  status.value = "checking";
  hint.value = null;
  positions.value = [];
  try {
    const s = await ibkrStatus();
    if (s.reachable && s.authenticated) {
      status.value = "connected";
      const p = await ibkrPositions();
      account.value = p.account;
      positions.value = p.positions;
    } else {
      status.value = "disconnected";
      hint.value = s.hint ?? "IBKR-gateway niet verbonden.";
    }
  } catch (e) {
    status.value = "disconnected";
    hint.value = String(e);
  }
}

onMounted(refresh);
</script>

<template>
  <section class="view">
    <h1>IBKR</h1>
    <p class="muted">
      Read-only posities via de Client Portal Gateway (paper of live). De login
      met SSO + 2FA doe je in de gateway; Jarvis leest alleen.
    </p>

    <div class="badge">
      <span
        class="dot"
        :class="status === 'connected' ? 'dot-ok' : status === 'disconnected' ? 'dot-err' : 'dot-todo'"
      ></span>
      {{
        status === "connected"
          ? `verbonden — account ${account}`
          : status === "disconnected"
            ? "niet verbonden"
            : "controleren…"
      }}
    </div>

    <p v-if="hint" class="muted">{{ hint }}</p>
    <button @click="refresh">Opnieuw verbinden</button>

    <ul v-if="positions.length" class="holding-list" style="margin-top: 18px">
      <li v-for="p in positions" :key="p.conid" class="holding-head">
        <span class="sym">{{ p.symbol }}</span>
        <span class="muted">{{ p.position }} @ {{ p.mkt_price }} {{ p.currency }}</span>
        <span class="cost">{{ p.mkt_value }}</span>
      </li>
    </ul>
  </section>
</template>
