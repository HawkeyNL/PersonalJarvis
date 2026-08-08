<script setup lang="ts">
import { ref, onMounted } from "vue";
import {
  listHoldings,
  addHolding,
  deleteHolding,
  type Holding,
} from "../portfolio";

const holdings = ref<Holding[]>([]);
const total = ref("0");
const symbol = ref("");
const quantity = ref("");
const avgCost = ref("");
const error = ref<string | null>(null);
const loading = ref(true);

async function refresh() {
  loading.value = true;
  error.value = null;
  try {
    const data = await listHoldings();
    holdings.value = data.holdings;
    total.value = data.total_cost;
  } catch (e) {
    error.value = String(e);
  } finally {
    loading.value = false;
  }
}

async function add() {
  error.value = null;
  try {
    await addHolding({
      symbol: symbol.value,
      quantity: quantity.value,
      avg_cost: avgCost.value,
    });
    symbol.value = "";
    quantity.value = "";
    avgCost.value = "";
    await refresh();
  } catch (e) {
    error.value = String(e);
  }
}

async function remove(id: string) {
  try {
    await deleteHolding(id);
    await refresh();
  } catch (e) {
    error.value = String(e);
  }
}

onMounted(refresh);
</script>

<template>
  <section class="view">
    <h1>Portfolio</h1>
    <p class="muted">
      Handmatige posities (nog geen live koersen). Bedragen in Decimal, achter
      je device-login.
    </p>

    <form class="holding-form" @submit.prevent="add">
      <input v-model="symbol" placeholder="Symbool (AAPL)" aria-label="symbool" />
      <input v-model="quantity" placeholder="Aantal" inputmode="decimal" aria-label="aantal" />
      <input v-model="avgCost" placeholder="Gem. kostprijs" inputmode="decimal" aria-label="kostprijs" />
      <button type="submit">Toevoegen</button>
    </form>

    <p v-if="error" class="muted err">{{ error }}</p>
    <p v-if="loading" class="muted">Laden…</p>

    <div v-else-if="holdings.length" class="holdings">
      <div class="total">Totale kostenbasis: <strong>{{ total }}</strong></div>
      <ul class="holding-list">
        <li v-for="h in holdings" :key="h.id" class="holding">
          <div class="holding-head">
            <span class="sym">{{ h.symbol }}</span>
            <span class="muted">{{ h.quantity }} × {{ h.avg_cost }} {{ h.currency }}</span>
            <span class="cost">{{ h.cost_basis }}</span>
            <button class="del" @click="remove(h.id)" aria-label="verwijderen">✕</button>
          </div>
          <div class="bar">
            <div class="bar-fill" :style="{ width: h.weight_pct + '%' }"></div>
          </div>
          <div class="weight muted">{{ h.weight_pct }}%</div>
        </li>
      </ul>
    </div>
    <p v-else-if="!loading" class="muted">
      Nog geen posities. Voeg er hierboven één toe.
    </p>
  </section>
</template>
