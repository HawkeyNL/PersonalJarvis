import { computed, readonly, ref } from "vue";
import { invoke } from "@tauri-apps/api/core";

export type Platform = "linux" | "macos" | "windows" | "unknown";

export interface PlatformCapabilities {
  platform: Platform;
  supportsBiometrics: boolean;
  supportsWakeWord: boolean;
  supportsBackgroundAudio: boolean;
  supportsNotifications: boolean;
  supportsLocalSecureStorage: boolean;
  supportsDesktopWindowFeatures: boolean;
}

export function capabilitiesFor(platform: Platform): PlatformCapabilities {
  const desktop = platform !== "unknown";
  return {
    platform,
    supportsBiometrics: platform === "macos" || platform === "windows",
    supportsWakeWord: desktop,
    supportsBackgroundAudio: false,
    supportsNotifications: false,
    supportsLocalSecureStorage: desktop,
    supportsDesktopWindowFeatures: desktop,
  };
}

const current = ref<PlatformCapabilities>(capabilitiesFor("unknown"));
let initialization: Promise<PlatformCapabilities> | null = null;

export const platformCapabilities = readonly(current);
export const isDesktop = computed(() => current.value.supportsDesktopWindowFeatures);

export function initPlatformCapabilities(): Promise<PlatformCapabilities> {
  if (initialization) return initialization;
  initialization = invoke<{ platform: string }>("device_info")
    .then(({ platform }) => {
      const supported: Platform = ["linux", "macos", "windows"].includes(platform)
        ? (platform as Platform)
        : "unknown";
      current.value = capabilitiesFor(supported);
      return current.value;
    })
    .catch(() => current.value);
  return initialization;
}
