<script setup lang="ts">
import { ref, computed, onMounted } from "vue";
import { useRoute } from "vue-router";
import {
  listHoldings,
  addHolding,
  deleteHolding,
  type Holding,
} from "../portfolio";
import { ibkrStatus, ibkrPositions, type IbkrPosition } from "../ibkr";

// Sub-tab is driven by the route (/trading vs /trading/ibkr).
const route = useRoute();
const seg = computed<"manual" | "ibkr">(() =>
  route.path.endsWith("/ibkr") ? "ibkr" : "manual",
);

// Manual holdings
const holdings = ref<Holding[]>([]);
const total = ref("0");
const symbol = ref("");
const quantity = ref("");
const avgCost = ref("");
const hError = ref<string | null>(null);
const loading = ref(true);

// IBKR
const ibStatus = ref<"checking" | "connected" | "disconnected">("checking");
const ibHint = ref<string | null>(null);
const account = ref<string | null>(null);
const positions = ref<IbkrPosition[]>([]);

async function refreshHoldings() {
  loading.value = true;
  hError.value = null;
  try {
    const data = await listHoldings();
    holdings.value = data.holdings;
    total.value = data.total_cost;
  } catch (e) {
    hError.value = String(e);
  } finally {
    loading.value = false;
  }
}

async function add() {
  hError.value = null;
  try {
    await addHolding({
      symbol: symbol.value,
      quantity: quantity.value,
      avg_cost: avgCost.value,
    });
    symbol.value = "";
    quantity.value = "";
    avgCost.value = "";
    await refreshHoldings();
  } catch (e) {
    hError.value = String(e);
  }
}

async function remove(id: string) {
  try {
    await deleteHolding(id);
    await refreshHoldings();
  } catch (e) {
    hError.value = String(e);
  }
}

async function refreshIbkr() {
  ibStatus.value = "checking";
  ibHint.value = null;
  positions.value = [];
  try {
    const s = await ibkrStatus();
    if (s.reachable && s.authenticated) {
      ibStatus.value = "connected";
      const p = await ibkrPositions();
      account.value = p.account;
      positions.value = p.positions;
    } else {
      ibStatus.value = "disconnected";
      ibHint.value = s.hint ?? "IBKR-gateway niet verbonden (start de Client Portal Gateway en log in).";
    }
  } catch (e) {
    ibStatus.value = "disconnected";
    ibHint.value = String(e);
  }
}

onMounted(() => {
  refreshHoldings();
  refreshIbkr();
});
</script>

<template>
  <section class="view trading">
    <header class="desk-top">
      <div>
        <h1>Trading</h1>
        <p class="muted sub">
          Je posities op één desk — handmatig en live via IBKR (read-only).
          Traden (orders) volgt later, met risk-checks en bevestiging.
        </p>
      </div>
    </header>

    <!-- summary tiles -->
    <div class="tiles">
      <div class="tile glass">
        <span class="t-k">POSITIES</span>
        <span class="t-v">{{ holdings.length }}</span>
      </div>
      <div class="tile glass">
        <span class="t-k">KOSTENBASIS</span>
        <span class="t-v mono">{{ total }}</span>
      </div>
      <div class="tile glass">
        <span class="t-k">IBKR</span>
        <span class="t-v small">
          <span class="dot" :class="ibStatus === 'connected' ? 'dot-ok' : ibStatus === 'disconnected' ? 'dot-err' : 'dot-todo'"></span>
          {{ ibStatus === "connected" ? "verbonden" : ibStatus === "disconnected" ? "offline" : "…" }}
        </span>
      </div>
    </div>

    <!-- MANUAL -->
    <div v-show="seg === 'manual'" class="panel glass">
      <form class="holding-form" @submit.prevent="add">
        <input v-model="symbol" placeholder="Symbool (AAPL)" aria-label="symbool" />
        <input v-model="quantity" placeholder="Aantal" inputmode="decimal" aria-label="aantal" />
        <input v-model="avgCost" placeholder="Gem. kostprijs" inputmode="decimal" aria-label="kostprijs" />
        <button type="submit">Toevoegen</button>
      </form>

      <p v-if="hError" class="muted err">{{ hError }}</p>
      <p v-if="loading" class="muted">Laden…</p>

      <div v-else-if="holdings.length" class="holdings">
        <div class="total">Totale kostenbasis: <strong>{{ total }}</strong></div>
        <ul class="holding-list">
          <li v-for="h in holdings" :key="h.id" class="holding">
            <div class="holding-head">
              <span class="sym">{{ h.symbol }}</span>
              <span class="muted">{{ h.quantity }} × {{ h.avg_cost }} {{ h.currency }}</span>
              <span class="cost mono">{{ h.cost_basis }}</span>
              <button class="del" @click="remove(h.id)" aria-label="verwijderen">✕</button>
            </div>
            <div class="bar"><div class="bar-fill" :style="{ width: h.weight_pct + '%' }"></div></div>
            <div class="weight muted">{{ h.weight_pct }}%</div>
          </li>
        </ul>
      </div>
      <p v-else-if="!loading" class="muted">Nog geen posities. Voeg er hierboven één toe.</p>
    </div>

    <!-- IBKR -->
    <div v-show="seg === 'ibkr'" class="panel glass">
      <div class="ib-head">
        <div class="badge">
          <span class="dot" :class="ibStatus === 'connected' ? 'dot-ok' : ibStatus === 'disconnected' ? 'dot-err' : 'dot-todo'"></span>
          {{
            ibStatus === "connected"
              ? `verbonden — account ${account}`
              : ibStatus === "disconnected"
                ? "niet verbonden"
                : "controleren…"
          }}
        </div>
        <button class="ghost" @click="refreshIbkr">Opnieuw verbinden</button>
      </div>

      <p class="muted sub">
        Read-only via de Client Portal Gateway (paper of live). De login met
        SSO + 2FA doe je in de gateway; Jarvis leest alleen.
      </p>
      <p v-if="ibHint" class="muted">{{ ibHint }}</p>

      <ul v-if="positions.length" class="holding-list" style="margin-top: 14px">
        <li v-for="p in positions" :key="p.conid" class="holding-head">
          <span class="sym">{{ p.symbol }}</span>
          <span class="muted">{{ p.position }} @ {{ p.mkt_price }} {{ p.currency }}</span>
          <span class="cost mono">{{ p.mkt_value }}</span>
        </li>
      </ul>
    </div>
  </section>
</template>

<style scoped>
.trading { width: 100%; }
.desk-top { margin-bottom: 18px; }
.sub { margin: 6px 0 0; font-size: 13px; }

.glass {
  background: rgba(14, 30, 22, 0.5);
  backdrop-filter: blur(14px) saturate(1.25);
  -webkit-backdrop-filter: blur(14px) saturate(1.25);
  border: 1px solid var(--border);
  box-shadow: inset 0 1px 0 rgba(255, 255, 255, 0.06);
}

.tiles { display: grid; grid-template-columns: repeat(3, 1fr); gap: 12px; margin-bottom: 18px; }
.tile {
  border-radius: 12px; padding: 12px 14px;
  display: flex; flex-direction: column; gap: 6px;
}
.t-k { font-family: var(--mono); font-size: 10px; letter-spacing: 0.16em; color: var(--muted); }
.t-v { font-size: 26px; font-weight: 700; color: #eafff4; font-variant-numeric: tabular-nums; }
.t-v.small { font-size: 15px; display: inline-flex; align-items: center; gap: 8px; }
.mono { font-family: var(--mono); }

.seg {
  display: inline-flex; gap: 4px; padding: 4px; margin-bottom: 16px;
  border-radius: 14px;
  background: linear-gradient(180deg, rgba(255,255,255,0.08), rgba(255,255,255,0.02));
  border: 1px solid rgba(255, 255, 255, 0.12);
  backdrop-filter: blur(16px) saturate(1.4);
  -webkit-backdrop-filter: blur(16px) saturate(1.4);
}
.seg button {
  background: transparent; color: var(--muted); border: none;
  padding: 8px 18px; border-radius: 10px; font-size: 13px; font-weight: 600; cursor: pointer;
}
.seg button:hover { color: var(--text); }
.seg button.on {
  color: var(--accent);
  background: linear-gradient(180deg, rgba(255,255,255,0.18), rgba(255,255,255,0.05));
  box-shadow: inset 0 1px 0 rgba(255,255,255,0.4), 0 2px 8px rgba(0,0,0,0.25);
}

.panel { border-radius: 14px; padding: 16px 18px 18px; }
.ib-head { display: flex; align-items: center; justify-content: space-between; gap: 12px; flex-wrap: wrap; }
.ib-head .badge { margin: 0; }
.ghost {
  background: transparent; border: 1px solid var(--border); color: var(--muted);
  font-family: var(--mono); font-size: 12px;
}
.ghost:hover { color: var(--accent-2); border-color: var(--accent-2); filter: none; }

@media (max-width: 560px) {
  .tiles { grid-template-columns: 1fr; }
}
</style>
