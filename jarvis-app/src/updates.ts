import { Channel, invoke } from "@tauri-apps/api/core";
import { computed, ref } from "vue";
import { AUTOMATIC_UPDATE_DELAY_MS, shouldScheduleAutomaticUpdateCheck } from "./updatePolicy.js";

type NativeUpdateState =
  | "ready"
  | "unconfigured"
  | "unauthenticated"
  | "unsupported"
  | "incompatible"
  | "unavailable"
  | "up_to_date"
  | "available"
  | "installed";

type NativeUpdateStatus = {
  state: NativeUpdateState;
  current_version: string;
  version: string | null;
  notes: string | null;
};

type DownloadEvent =
  | { event: "started"; data: { content_length: number | null } }
  | { event: "progress"; data: { chunk_length: number } }
  | { event: "finished" };

export type UpdateUiState = NativeUpdateState | "idle" | "checking" | "downloading" | "installing" | "error";

export const updateState = ref<UpdateUiState>("idle");
export const currentAppVersion = ref("0.1.0");
export const availableAppVersion = ref<string | null>(null);
export const updateNotes = ref<string | null>(null);
export const updateError = ref<string | null>(null);
export const updateProgress = ref<number | null>(null);

export const updateBusy = computed(() =>
  ["checking", "downloading", "installing"].includes(updateState.value),
);

function apply(status: NativeUpdateStatus) {
  updateState.value = status.state;
  currentAppVersion.value = status.current_version;
  availableAppVersion.value = status.version;
  updateNotes.value = status.notes;
}

export async function loadUpdateStatus(): Promise<void> {
  try {
    apply(await invoke<NativeUpdateStatus>("app_update_status"));
  } catch {
    updateState.value = "unsupported";
  }
}

export async function checkForUpdate(): Promise<void> {
  updateState.value = "checking";
  updateError.value = null;
  updateProgress.value = null;
  try {
    apply(await invoke<NativeUpdateStatus>("app_update_check"));
  } catch {
    updateState.value = "error";
    updateError.value = "De privé-updateservice is momenteel niet bereikbaar. Jarvis blijft gewoon bruikbaar.";
  }
}

export async function installAvailableUpdate(): Promise<void> {
  updateState.value = "downloading";
  updateError.value = null;
  let downloaded = 0;
  let total: number | null = null;
  const onEvent = new Channel<DownloadEvent>();
  onEvent.onmessage = (message) => {
    if (message.event === "started") {
      total = message.data.content_length;
      updateProgress.value = total ? 0 : null;
    } else if (message.event === "progress") {
      downloaded += message.data.chunk_length;
      updateProgress.value = total ? Math.min(100, Math.round((downloaded / total) * 100)) : null;
    } else {
      updateState.value = "installing";
      updateProgress.value = 100;
    }
  };
  try {
    apply(await invoke<NativeUpdateStatus>("app_update_install", { onEvent }));
  } catch {
    updateState.value = "error";
    updateError.value = "Download, handtekeningcontrole of installatie is mislukt. De huidige versie is niet gewijzigd.";
  }
}

export function restartAfterUpdate(): Promise<void> {
  return invoke("app_update_restart");
}

let automaticCheckScheduled = false;

/** Run at most one gentle, non-blocking check after an authenticated startup.
 * Installation and restart always remain explicit user actions. */
export function scheduleAutomaticUpdateCheck(configured: boolean, authenticated: boolean): void {
  if (!shouldScheduleAutomaticUpdateCheck(configured, authenticated, automaticCheckScheduled)) return;
  automaticCheckScheduled = true;
  window.setTimeout(() => {
    void (async () => {
      await loadUpdateStatus();
      if (updateState.value === "ready") await checkForUpdate();
    })();
  }, AUTOMATIC_UPDATE_DELAY_MS);
}
