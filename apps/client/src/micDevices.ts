// Microphone selection, shared by voice capture (enroll/verify) and the wake
// detector. The chosen device id is persisted; capture falls back to the system
// default when none is chosen or the device is gone.
//
// Note: device *labels* are only exposed by the browser after mic permission has
// been granted once — so the list fills in after the first recording.
import { ref } from "vue";

const MIC_KEY = "jarvis.mic.deviceId";

export interface MicDevice {
  deviceId: string;
  label: string;
}

export const selectedMic = ref<string>(localStorage.getItem(MIC_KEY) ?? "");
export const mics = ref<MicDevice[]>([]);

export function setMic(id: string): void {
  selectedMic.value = id;
  if (id) localStorage.setItem(MIC_KEY, id);
  else localStorage.removeItem(MIC_KEY);
}

/** Enumerate audio-input devices (labels appear once permission is granted). */
export async function listMics(): Promise<MicDevice[]> {
  if (!navigator.mediaDevices?.enumerateDevices) {
    mics.value = [];
    return [];
  }
  const devices = await navigator.mediaDevices.enumerateDevices();
  mics.value = devices
    .filter((d) => d.kind === "audioinput")
    .map((d, i) => ({ deviceId: d.deviceId, label: d.label || `Microfoon ${i + 1}` }));
  // If the remembered device vanished, drop back to the default.
  if (selectedMic.value && !mics.value.some((m) => m.deviceId === selectedMic.value)) {
    setMic("");
  }
  return mics.value;
}

/** getUserMedia constraints honoring the chosen device, else the default. */
export function micConstraints(): MediaStreamConstraints {
  const id = selectedMic.value;
  return { audio: id ? { deviceId: { exact: id } } : true };
}
