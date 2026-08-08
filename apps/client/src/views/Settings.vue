<script setup lang="ts">
import { ref, onMounted } from "vue";
import { API_BASE } from "../api";
import { ACCENTS, PRESETS, currentAccent, applyAccent, type Accent } from "../theme";
import { currentSession, logout, type Session } from "../auth";

const accent = ref<Accent>(currentAccent());
const session = ref<Session | null>(null);

const labels: Record<Accent, string> = {
  green: "Jarvis-groen",
  cyan: "Cyaan",
  amber: "Amber",
  violet: "Violet",
};

function pick(a: Accent) {
  accent.value = a;
  applyAccent(a);
}

async function doLogout() {
  await logout();
  session.value = await currentSession();
}

onMounted(async () => {
  session.value = await currentSession();
});
</script>

<template>
  <section class="view settings">
    <h1>Settings</h1>

    <div class="panel glass">
      <div class="panel-head">WEERGAVE <span class="hint">accentkleur</span></div>
      <div class="swatches">
        <button
          v-for="a in ACCENTS"
          :key="a"
          class="swatch"
          :class="{ on: accent === a }"
          :style="{ '--c1': PRESETS[a][0], '--c2': PRESETS[a][1] }"
          @click="pick(a)"
        >
          <span class="chip-dot"></span>
          {{ labels[a] }}
        </button>
      </div>
      <p class="muted small">Groen is de standaardkleur van Jarvis.</p>
    </div>

    <div class="panel glass">
      <div class="panel-head">ACCOUNT <span class="hint">device-bound</span></div>
      <ul class="kv">
        <li>
          <span class="k">Status</span>
          <span class="v">
            <span class="dot" :class="session?.token ? 'dot-ok' : 'dot-todo'"></span>
            {{ session?.token ? "ingelogd" : "uitgelogd" }}
          </span>
        </li>
        <li>
          <span class="k">Apparaat-ID</span>
          <span class="v mono">{{ session?.device_id ?? "—" }}</span>
        </li>
        <li>
          <span class="k">Sleutel</span>
          <span class="v">{{ session?.has_key ? "in keychain" : "geen" }}</span>
        </li>
      </ul>
      <button v-if="session?.token" class="ghost" @click="doLogout">Uitloggen</button>
    </div>

    <div class="panel glass">
      <div class="panel-head">SYSTEEM <span class="hint">info</span></div>
      <ul class="kv">
        <li><span class="k">Backend</span><span class="v mono">{{ API_BASE }}</span></li>
        <li><span class="k">Client</span><span class="v mono">Jarvis MK I · v0.1.0</span></li>
        <li><span class="k">Broker</span><span class="v">IBKR read-only</span></li>
      </ul>
    </div>
  </section>
</template>

<style scoped>
.settings { max-width: 640px; }
.panel {
  position: relative;
  border: 1px solid var(--border);
  border-radius: 12px;
  padding: 14px 16px 16px;
  margin-bottom: 16px;
}
.glass {
  background: rgba(14, 30, 22, 0.5);
  backdrop-filter: blur(14px) saturate(1.3);
  -webkit-backdrop-filter: blur(14px) saturate(1.3);
}
.panel-head {
  display: flex; align-items: center; justify-content: space-between;
  font-family: var(--mono); font-size: 11px; letter-spacing: 0.18em;
  color: var(--accent); margin-bottom: 12px;
}
.hint { font-size: 9px; color: var(--muted); letter-spacing: 0.1em; }

.swatches { display: flex; flex-wrap: wrap; gap: 10px; }
.swatch {
  display: inline-flex; align-items: center; gap: 8px;
  background: rgba(255, 255, 255, 0.03);
  border: 1px solid var(--border); color: var(--text);
  border-radius: 999px; padding: 7px 14px; font-size: 13px; cursor: pointer;
  font-weight: 500;
}
.swatch .chip-dot {
  width: 12px; height: 12px; border-radius: 50%;
  background: linear-gradient(135deg, var(--c1), var(--c2));
  box-shadow: 0 0 8px var(--c1);
}
.swatch.on { border-color: var(--c1); box-shadow: 0 0 0 1px var(--c1), 0 0 14px rgba(255,255,255,0.06); }
.swatch:hover { border-color: var(--c1); }

.small { font-size: 12px; margin: 12px 0 0; }

.kv { list-style: none; margin: 0; padding: 0; display: flex; flex-direction: column; gap: 10px; }
.kv li { display: flex; align-items: center; gap: 12px; }
.kv .k { font-size: 13px; color: var(--muted); }
.kv .v { margin-left: auto; font-size: 13px; display: inline-flex; align-items: center; gap: 8px; }
.mono { font-family: var(--mono); font-size: 12px; }
.ghost {
  margin-top: 14px; background: transparent; border: 1px solid var(--border);
  color: var(--muted); font-family: var(--mono); font-size: 12px;
}
.ghost:hover { color: var(--accent-2); border-color: var(--accent-2); filter: none; }
</style>
