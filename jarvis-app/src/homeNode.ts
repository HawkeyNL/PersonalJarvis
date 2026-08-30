import { invoke } from "@tauri-apps/api/core";
import { readonly, ref } from "vue";

/** Ordinary, non-secret connection metadata for this enrolled device. */
export type HomeNodeConfig = {
  origin: string | null;
  configured: boolean;
};

const config = ref<HomeNodeConfig>({ origin: null, configured: false });
let loaded = false;
let loading: Promise<HomeNodeConfig> | null = null;

export const homeNodeConfig = readonly(config);

export class HomeNodeUnconfiguredError extends Error {
  constructor() {
    super("Home Node is niet geconfigureerd");
    this.name = "HomeNodeUnconfiguredError";
  }
}

export async function loadHomeNodeConfig(): Promise<HomeNodeConfig> {
  if (loaded) return config.value;
  if (!loading) {
    loading = invoke<HomeNodeConfig>("home_node_config")
      .then((value) => {
        config.value = value;
        loaded = true;
        return value;
      })
      .finally(() => {
        loading = null;
      });
  }
  return loading;
}

/** Validate and persist the origin before enrollment starts. Credentials are
 * rejected native-side and are never accepted as part of this metadata. */
export async function configureHomeNode(origin: string): Promise<HomeNodeConfig> {
  const value = await invoke<HomeNodeConfig>("home_node_configure", { origin });
  config.value = value;
  loaded = true;
  return value;
}

/** Resolve the active origin for every request, rather than capturing an
 * immutable build-time endpoint. */
export async function homeNodeOrigin(): Promise<string> {
  const value = await loadHomeNodeConfig();
  if (!value.configured || !value.origin) throw new HomeNodeUnconfiguredError();
  return value.origin;
}
