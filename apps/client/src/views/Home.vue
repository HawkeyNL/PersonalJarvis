<script setup lang="ts">
import { ref, computed, onMounted, onBeforeUnmount } from "vue";
import { getJson } from "../api";
import {
  currentSession,
  login,
  logout,
  listDevices,
  type DeviceItem,
} from "../auth";
import { listHoldings } from "../portfolio";
import { ibkrStatus, type IbkrStatus } from "../ibkr";
import ReactorCore from "../components/ReactorCore.vue";

type State = "checking" | "ok" | "fout";

const backend = ref<State>("checking");
const auth = ref<"checking" | "in" | "uit" | "fout">("checking");
const devices = ref<DeviceItem[]>([]);
const portfolioTotal = ref("0");
const portfolioCount = ref(0);
const ibkr = ref<IbkrStatus | null>(null);
const error = ref<string | null>(null);

const clock = ref("--:--:--");
const uptime = ref(0);
const feed = ref<{ id: number; t: string; kind: string; msg: string }[]>([]);
let feedId = 0;

let clockTimer: number | undefined;
let pollTimer: number | undefined;

function pad(n: number): string {
  return String(n).padStart(2, "0");
}

function fmtClock(d: Date): string {
  return `${pad(d.getHours())}:${pad(d.getMinutes())}:${pad(d.getSeconds())}`;
}

function pushFeed(kind: string, msg: string) {
  feed.value.unshift({ id: feedId++, t: fmtClock(new Date()), kind, msg });
  if (feed.value.length > 8) feed.value.pop();
}

const online = computed(() => backend.value === "ok");
const uptimeStr = computed(() => {
  const h = Math.floor(uptime.value / 3600);
  const m = Math.floor((uptime.value % 3600) / 60);
  const s = uptime.value % 60;
  return `${pad(h)}:${pad(m)}:${pad(s)}`;
});

// Meter widths reflect real-ish state (0-100).
const meters = computed(() => [
  { key: "LINK", pct: backend.value === "ok" ? 96 : 8 },
  { key: "AUTH", pct: auth.value === "in" ? 100 : 12 },
  { key: "MESH", pct: Math.min(100, 20 + devices.value.length * 26) },
  { key: "BROKER", pct: ibkr.value?.authenticated ? 100 : ibkr.value?.reachable ? 45 : 6 },
]);

const ibkrLabel = computed(() => {
  const s = ibkr.value;
  if (!s) return "controleren…";
  if (s.authenticated) return "verbonden (ingelogd)";
  if (s.reachable) return "gateway bereikbaar · niet ingelogd";
  return "gateway offline";
});

async function pollBackend() {
  try {
    await getJson("/readyz");
    if (backend.value !== "ok") pushFeed("ok", "LINK backend /readyz → 200 OK");
    backend.value = "ok";
  } catch {
    if (backend.value !== "fout") pushFeed("err", "LINK backend onbereikbaar");
    backend.value = "fout";
  }
}

async function refreshAuthData() {
  try {
    let session = await currentSession();
    if (!session.token) {
      await login();
      session = await currentSession();
      if (session.token) pushFeed("ok", "AUTH device-bound sessie geactiveerd");
    }
    if (session.token) {
      devices.value = await listDevices(session.token);
      auth.value = "in";
      try {
        const p = await listHoldings();
        portfolioTotal.value = p.total_cost;
        portfolioCount.value = p.holdings.length;
        pushFeed("info", `PORTFOLIO ${p.holdings.length} posities · basis ${p.total_cost}`);
      } catch {
        /* portfolio summary is optional */
      }
      try {
        ibkr.value = await ibkrStatus();
        pushFeed(
          ibkr.value.reachable ? "info" : "warn",
          `IBKR ${ibkr.value.authenticated ? "ingelogd" : ibkr.value.reachable ? "gateway bereikbaar" : "offline"}`,
        );
      } catch {
        /* ibkr optional */
      }
    } else {
      auth.value = "uit";
    }
  } catch (e) {
    auth.value = "fout";
    error.value = String(e);
    pushFeed("err", "AUTH mislukt");
  }
}

async function boot() {
  pushFeed("info", "JARVIS neural core online");
  await pollBackend();
  await refreshAuthData();
}

async function doLogout() {
  await logout();
  devices.value = [];
  auth.value = "uit";
  pushFeed("warn", "AUTH sessie beëindigd");
}

onMounted(() => {
  clockTimer = window.setInterval(() => {
    clock.value = fmtClock(new Date());
    uptime.value += 1;
  }, 1000);
  clock.value = fmtClock(new Date());
  pollTimer = window.setInterval(pollBackend, 5000);
  boot();
});

onBeforeUnmount(() => {
  clearInterval(clockTimer);
  clearInterval(pollTimer);
});
</script>

<template>
  <section class="hud">
    <header class="hud-top">
      <div class="hud-brand">
        <span class="pulse" :class="{ off: !online }"></span>
        <span class="wordmark">JARVIS</span>
        <small>MK I</small>
      </div>
      <div class="hud-modes">
        <span class="chip chip-on">SYSTEM</span>
        <RouterLink to="/portfolio" class="chip">TRADING</RouterLink>
      </div>
      <div class="hud-engine">
        <span class="dot" :class="online ? 'dot-ok' : backend === 'fout' ? 'dot-err' : 'dot-todo'"></span>
        ENGINE {{ online ? "ONLINE" : backend === "fout" ? "OFFLINE" : "…" }}
        <b class="clock">{{ clock }}</b>
      </div>
    </header>

    <div class="hud-grid">
      <!-- LEFT: system telemetry -->
      <div class="col">
        <div class="panel">
          <div class="panel-head">SYSTEM STATUS <span class="hint">live</span></div>
          <ul class="tel">
            <li>
              <span class="dot" :class="online ? 'dot-ok' : backend === 'fout' ? 'dot-err' : 'dot-todo'"></span>
              <span class="k">BACKEND</span>
              <span class="v">{{ online ? "verbonden" : backend === "fout" ? "onbereikbaar" : "controleren…" }}</span>
            </li>
            <li>
              <span class="dot" :class="auth === 'in' ? 'dot-ok' : auth === 'fout' ? 'dot-err' : 'dot-todo'"></span>
              <span class="k">LOGIN</span>
              <span class="v">{{ auth === "in" ? "device-bound" : auth === "fout" ? "mislukt" : auth === "uit" ? "uitgelogd" : "bezig…" }}</span>
            </li>
            <li>
              <span class="dot dot-ok"></span>
              <span class="k">UPTIME</span>
              <span class="v mono">{{ uptimeStr }}</span>
            </li>
          </ul>
        </div>

        <div class="panel">
          <div class="panel-head">DEVICE MESH <span class="hint">{{ devices.length }}</span></div>
          <ul class="tel" v-if="devices.length">
            <li v-for="d in devices" :key="d.id">
              <span class="dot dot-ok"></span>
              <span class="k">{{ d.name }}</span>
              <span class="v mono">{{ d.platform }}</span>
            </li>
          </ul>
          <p v-else class="empty">geen apparaten gekoppeld</p>
        </div>
      </div>

      <!-- CENTER: reactor core -->
      <div class="col col-core">
        <ReactorCore name="Jarvis" :active="online" />
        <div class="meters">
          <div class="meter" v-for="m in meters" :key="m.key">
            <span class="meter-k">{{ m.key }}</span>
            <span class="meter-bar"><i :style="{ width: m.pct + '%' }"></i></span>
          </div>
        </div>
        <div class="core-actions">
          <button v-if="auth === 'in'" class="ghost" @click="doLogout">Uitloggen</button>
          <button v-else-if="auth === 'uit'" @click="boot">Inloggen</button>
        </div>
      </div>

      <!-- RIGHT: portfolio, broker, feed -->
      <div class="col">
        <div class="panel">
          <div class="panel-head">PORTFOLIO <span class="hint">basis</span></div>
          <div class="big-stat">
            <span class="num">{{ portfolioCount }}</span>
            <span class="unit">posities</span>
          </div>
          <div class="sub-stat mono">kostenbasis {{ portfolioTotal }}</div>
        </div>

        <div class="panel">
          <div class="panel-head">
            IBKR LINK
            <span class="hint" :class="ibkr?.authenticated ? 'ok' : 'warn'">read-only</span>
          </div>
          <div class="tel">
            <div class="li">
              <span class="dot" :class="ibkr?.authenticated ? 'dot-ok' : ibkr?.reachable ? 'dot-todo' : 'dot-err'"></span>
              <span class="v">{{ ibkrLabel }}</span>
            </div>
          </div>
        </div>

        <div class="panel feed-panel">
          <div class="panel-head">LIVE ENGINE FEED <span class="hint">live</span></div>
          <ul class="feed">
            <li v-for="f in feed" :key="f.id" :class="'f-' + f.kind">
              <span class="ft">{{ f.t }}</span>
              <span class="fm">{{ f.msg }}</span>
            </li>
          </ul>
        </div>
      </div>
    </div>

    <p v-if="error" class="hud-err">Laatste fout: {{ error }}</p>
  </section>
</template>

<style scoped>
.hud {
  position: relative;
  min-height: calc(100vh - 64px);
}

/* faint grid + scanlines behind everything */
.hud::before {
  content: "";
  position: fixed;
  inset: 0;
  z-index: 0;
  pointer-events: none;
  background-image:
    linear-gradient(rgba(52, 245, 160, 0.04) 1px, transparent 1px),
    linear-gradient(90deg, rgba(52, 245, 160, 0.04) 1px, transparent 1px);
  background-size: 44px 44px;
  mask-image: radial-gradient(ellipse at 60% 40%, #000 30%, transparent 80%);
  -webkit-mask-image: radial-gradient(ellipse at 60% 40%, #000 30%, transparent 80%);
}

.hud > * { position: relative; z-index: 1; }

.hud-top {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 16px;
  padding-bottom: 16px;
  border-bottom: 1px solid var(--border);
  flex-wrap: wrap;
}

.hud-brand {
  display: flex;
  align-items: center;
  gap: 10px;
}

.wordmark {
  font-weight: 700;
  letter-spacing: 0.42em;
  font-size: 18px;
  color: #eafff4;
  text-shadow: 0 0 14px rgba(52, 245, 160, 0.35);
}

.hud-brand small {
  font-family: var(--mono);
  font-size: 10px;
  color: var(--muted);
  letter-spacing: 0.2em;
}

.pulse {
  width: 9px;
  height: 9px;
  border-radius: 50%;
  background: var(--accent);
  box-shadow: 0 0 0 0 rgba(52, 245, 160, 0.6);
  animation: ping 2s ease-out infinite;
}
.pulse.off { background: #f0a848; animation: none; }

.hud-modes { display: flex; gap: 8px; }

.chip {
  font-family: var(--mono);
  font-size: 11px;
  letter-spacing: 0.16em;
  padding: 5px 12px;
  border: 1px solid var(--border);
  border-radius: 6px;
  color: var(--muted);
  text-decoration: none;
  cursor: pointer;
}
.chip:hover { color: var(--text); border-color: var(--accent); }
.chip-on {
  color: #04140c;
  background: var(--accent);
  border-color: var(--accent);
  box-shadow: 0 0 14px rgba(52, 245, 160, 0.35);
}

.hud-engine {
  display: inline-flex;
  align-items: center;
  gap: 8px;
  font-family: var(--mono);
  font-size: 11px;
  letter-spacing: 0.14em;
  color: var(--muted);
}
.clock { color: var(--accent-2); margin-left: 6px; }

.hud-grid {
  display: grid;
  grid-template-columns: minmax(200px, 1fr) minmax(320px, 1.4fr) minmax(220px, 1fr);
  gap: 18px;
  margin-top: 20px;
  align-items: start;
}

.col { display: flex; flex-direction: column; gap: 16px; }
.col-core {
  align-items: center;
  justify-content: center;
  gap: 22px;
  --core-size: clamp(320px, 46vh, 640px);
}

.panel {
  position: relative;
  width: 100%;
  background: linear-gradient(180deg, rgba(16, 34, 26, 0.5), rgba(8, 18, 14, 0.42));
  backdrop-filter: blur(12px) saturate(1.25);
  -webkit-backdrop-filter: blur(12px) saturate(1.25);
  border: 1px solid var(--border);
  border-radius: 12px;
  padding: 12px 14px 14px;
  box-shadow: inset 0 1px 0 rgba(255, 255, 255, 0.06);
}
/* corner brackets */
.panel::before,
.panel::after {
  content: "";
  position: absolute;
  width: 12px;
  height: 12px;
  border: 1px solid var(--accent);
  opacity: 0.7;
}
.panel::before { top: -1px; left: -1px; border-right: 0; border-bottom: 0; }
.panel::after { bottom: -1px; right: -1px; border-left: 0; border-top: 0; }

.panel-head {
  display: flex;
  align-items: center;
  justify-content: space-between;
  font-family: var(--mono);
  font-size: 11px;
  letter-spacing: 0.18em;
  color: var(--accent);
  margin-bottom: 10px;
}
.hint { font-size: 9px; color: var(--muted); letter-spacing: 0.1em; }
.hint.ok { color: var(--accent); }
.hint.warn { color: #f0a848; }

.tel { list-style: none; margin: 0; padding: 0; display: flex; flex-direction: column; gap: 8px; }
.tel li, .tel .li { display: flex; align-items: center; gap: 9px; }
.tel .k { font-size: 12px; color: var(--muted); letter-spacing: 0.06em; }
.tel .v { margin-left: auto; font-size: 12px; color: var(--text); }
.mono { font-family: var(--mono); }
.empty { font-size: 12px; color: var(--muted); margin: 2px 0 0; }

.big-stat { display: flex; align-items: baseline; gap: 8px; }
.big-stat .num {
  font-size: 40px; font-weight: 700; color: #eafff4;
  text-shadow: 0 0 14px rgba(52, 245, 160, 0.35); font-variant-numeric: tabular-nums;
}
.big-stat .unit { font-family: var(--mono); font-size: 11px; color: var(--muted); letter-spacing: 0.14em; }
.sub-stat { font-size: 11px; color: var(--muted); margin-top: 4px; }

.meters { width: var(--core-size, min(46vh, 400px)); display: flex; flex-direction: column; gap: 7px; }
.meter { display: flex; align-items: center; gap: 10px; }
.meter-k { font-family: var(--mono); font-size: 10px; color: var(--muted); width: 58px; letter-spacing: 0.12em; }
.meter-bar { flex: 1; height: 6px; background: rgba(52, 245, 160, 0.08); border-radius: 4px; overflow: hidden; }
.meter-bar i {
  display: block; height: 100%;
  background: linear-gradient(90deg, var(--accent), var(--accent-2));
  box-shadow: 0 0 10px rgba(52, 245, 160, 0.5);
  transition: width 0.6s ease;
}

.core-actions { min-height: 20px; }
button {
  font-family: var(--mono); font-size: 12px; letter-spacing: 0.08em;
}
.ghost { background: transparent; border: 1px solid var(--border); color: var(--muted); }
.ghost:hover { color: var(--accent-2); border-color: var(--accent-2); filter: none; }

.feed-panel { flex: 1; }
.feed { list-style: none; margin: 0; padding: 0; display: flex; flex-direction: column; gap: 5px; }
.feed li { display: flex; gap: 8px; font-family: var(--mono); font-size: 11px; line-height: 1.35; }
.feed .ft { color: var(--muted); flex: none; }
.feed .fm { color: var(--text); }
.f-ok .fm { color: var(--accent); }
.f-info .fm { color: var(--accent-2); }
.f-warn .fm { color: #f0a848; }
.f-err .fm { color: #f87171; }

.hud-err { margin-top: 16px; color: #f87171; font-size: 12px; }

@media (max-width: 980px) {
  .hud-grid { grid-template-columns: 1fr; }
  .col-core { order: -1; }
  .meters, .core { width: min(70vw, 360px); }
}

@keyframes ping {
  0% { box-shadow: 0 0 0 0 rgba(52, 245, 160, 0.5); }
  70% { box-shadow: 0 0 0 10px rgba(52, 245, 160, 0); }
  100% { box-shadow: 0 0 0 0 rgba(52, 245, 160, 0); }
}
</style>
