<script setup lang="ts">
import { ref, onMounted } from "vue";
import { API_BASE } from "../api";
import { ACCENTS, PRESETS, currentAccent, applyAccent, type Accent } from "../theme";
import { currentSession, deregisterDevice, type Session } from "../auth";
import { lockEnabled, isDesktop, setLockEnabled, initLock } from "../lock";
import {
  voiceSupported,
  enrolled,
  engine,
  busy,
  voiceStatus,
  voiceError,
  lastVerify,
  refreshVoiceStatus,
  enroll,
  verify,
} from "../voiceServer";
import {
  wakeEnabled,
  wakeStatus,
  wakeError,
  wakeReady,
  setWakeEnabled,
} from "../voicewake";

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

const confirmUnlink = ref(false);
const unlinking = ref(false);

async function doUnlink() {
  unlinking.value = true;
  try {
    await deregisterDevice();
    session.value = await currentSession();
  } finally {
    unlinking.value = false;
    confirmUnlink.value = false;
  }
}

onMounted(async () => {
  session.value = await currentSession();
  await initLock(); // resolves isDesktop for the lock toggle
  await refreshVoiceStatus();
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
      <button
        v-if="session?.token"
        class="ghost danger"
        @click="confirmUnlink = true"
      >
        Dit apparaat loskoppelen
      </button>
    </div>

    <div class="panel glass">
      <div class="panel-head">BEVEILIGING <span class="hint">app-vergrendeling</span></div>
      <label class="toggle" :class="{ off: !isDesktop }">
        <span class="tl">
          <span class="tt">Vergrendel bij openen</span>
          <span class="td">
            Touch ID / Face ID bij het openen. Faalt de biometrie, dan keur je
            goed via je telefoon.
          </span>
        </span>
        <input
          type="checkbox"
          :checked="lockEnabled"
          :disabled="!isDesktop"
          @change="setLockEnabled(($event.target as HTMLInputElement).checked)"
        />
        <span class="sw"></span>
      </label>
      <p v-if="!isDesktop" class="muted small">
        Dit toestel keurt ontgrendelingen goed voor je desktop — geen eigen slot nodig.
      </p>
    </div>

    <div class="panel glass">
      <div class="panel-head">STEM <span class="hint">server-side · centraal</span></div>
      <ul class="kv">
        <li>
          <span class="k">Stemprofiel</span>
          <span class="v">
            <span class="dot" :class="enrolled ? 'dot-ok' : 'dot-todo'"></span>
            {{ enrolled ? "ingeschreven" : "nog niet" }}
          </span>
        </li>
        <li v-if="engine">
          <span class="k">Engine</span>
          <span class="v mono">{{ engine }}</span>
        </li>
      </ul>

      <div class="enroll">
        <button class="ghost" :disabled="!voiceSupported || busy" @click="enroll()">
          {{ enrolled ? "Stem opnieuw opnemen" : "Neem je stem op" }}
        </button>
        <button
          v-if="enrolled"
          class="ghost"
          :disabled="!voiceSupported || busy"
          @click="verify()"
        >
          Test verificatie
        </button>
      </div>

      <p v-if="!voiceSupported" class="small errc">
        Microfoon niet beschikbaar op dit toestel.
      </p>
      <p v-else-if="voiceStatus" class="small" :class="busy ? 'muted' : 'okc'">
        {{ voiceStatus }}
      </p>
      <p v-if="voiceError" class="small errc">{{ voiceError }}</p>

      <p v-if="lastVerify && lastVerify.enrolled && lastVerify.transcript" class="small muted">
        Gehoord: "{{ lastVerify.transcript }}"
      </p>

      <label class="toggle" :class="{ off: !wakeReady }" style="margin-top: 14px">
        <span class="tl">
          <span class="tt">Luister naar "Hey Jarvis"</span>
          <span class="td">
            Werkt op elk toestel (in de app zelf). Alleen jouw stem wekt Jarvis.
            Tot de auto-detectie er is: ⌘⇧J als test-trigger.
          </span>
        </span>
        <input
          type="checkbox"
          :checked="wakeEnabled"
          :disabled="!wakeReady"
          @change="setWakeEnabled(($event.target as HTMLInputElement).checked)"
        />
        <span class="sw"></span>
      </label>
      <p class="small" :class="wakeError ? 'errc' : 'muted'">
        {{ wakeError || "status: " + wakeStatus }}
      </p>

      <p class="muted small">
        Je stem staat centraal op je eigen server en geldt op al je toestellen.
        Stem is gemak — Touch ID / je telefoon blijft het slot.
      </p>
    </div>

    <div class="panel glass">
      <div class="panel-head">SYSTEEM <span class="hint">info</span></div>
      <ul class="kv">
        <li><span class="k">Backend</span><span class="v mono">{{ API_BASE }}</span></li>
        <li><span class="k">Client</span><span class="v mono">Jarvis MK I · v0.1.0</span></li>
        <li><span class="k">Broker</span><span class="v">IBKR read-only</span></li>
      </ul>
    </div>

    <!-- Destructive-action confirmation. -->
    <Transition name="modal">
      <div v-if="confirmUnlink" class="modal-overlay" @click.self="confirmUnlink = false">
        <div class="modal glass" role="dialog" aria-modal="true" aria-labelledby="unlink-title">
          <h2 id="unlink-title">Dit apparaat loskoppelen?</h2>
          <p>
            Dit trekt dit toestel serverside in en wist de device-sleutel hier.
            Je moet dit apparaat daarna opnieuw koppelen (enrollen) om weer in te
            loggen. Je gegevens op de server blijven bestaan.
          </p>
          <div class="modal-actions">
            <button class="ghost" :disabled="unlinking" @click="confirmUnlink = false">
              Annuleren
            </button>
            <button class="ghost danger" :disabled="unlinking" @click="doUnlink">
              {{ unlinking ? "Loskoppelen…" : "Loskoppelen" }}
            </button>
          </div>
        </div>
      </div>
    </Transition>
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
.ghost.danger { color: #f87171; border-color: rgba(248, 113, 113, 0.4); }
.ghost.danger:hover { color: #fca5a5; border-color: #f87171; }
.ghost:disabled { opacity: 0.5; cursor: default; }

/* Destructive confirmation modal. */
.modal-overlay {
  position: fixed; inset: 0; z-index: 50;
  display: flex; align-items: center; justify-content: center; padding: 20px;
  background: rgba(0, 0, 0, 0.55);
  backdrop-filter: blur(4px); -webkit-backdrop-filter: blur(4px);
}
.modal {
  width: min(420px, 100%);
  border: 1px solid var(--border); border-radius: 14px; padding: 20px;
  box-shadow: 0 24px 70px rgba(0, 0, 0, 0.6);
}
.modal h2 { margin: 0 0 10px; font-size: 16px; }
.modal p { margin: 0; font-size: 13px; color: var(--muted); line-height: 1.5; }
.modal-actions { display: flex; justify-content: flex-end; gap: 10px; margin-top: 18px; }
.modal-actions .ghost { margin-top: 0; }

.modal-enter-active, .modal-leave-active { transition: opacity 0.2s ease; }
.modal-enter-from, .modal-leave-to { opacity: 0; }
.modal-enter-active .modal, .modal-leave-active .modal { transition: transform 0.2s ease; }
.modal-enter-from .modal, .modal-leave-to .modal { transform: translateY(8px) scale(0.98); }

.toggle { display: flex; align-items: center; gap: 14px; cursor: pointer; }
.toggle.off { cursor: default; }
.toggle .tl { display: flex; flex-direction: column; gap: 3px; }
.tt { font-size: 14px; }
.td { font-size: 12px; color: var(--muted); line-height: 1.4; max-width: 42ch; }
.toggle input { position: absolute; opacity: 0; pointer-events: none; }
.sw {
  margin-left: auto; flex: none; position: relative;
  width: 46px; height: 27px; border-radius: 999px;
  background: rgba(255, 255, 255, 0.08); border: 1px solid var(--border);
  transition: background 0.2s ease, border-color 0.2s ease;
}
.sw::after {
  content: ""; position: absolute; top: 2px; left: 2px;
  width: 21px; height: 21px; border-radius: 50%;
  background: var(--muted); transition: transform 0.2s ease, background 0.2s ease;
}
.toggle input:checked ~ .sw { background: rgba(52, 245, 160, 0.25); border-color: var(--accent); }
.toggle input:checked ~ .sw::after { transform: translateX(19px); background: var(--accent); }
.toggle input:disabled ~ .sw { opacity: 0.5; }

.enroll { display: flex; align-items: center; gap: 12px; margin-top: 12px; flex-wrap: wrap; }
.enroll .ghost { margin-top: 0; }
.okc { color: var(--accent); }
.errc { color: #f87171; }
</style>
