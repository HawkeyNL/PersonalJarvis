<script setup lang="ts">
import { onMounted, ref } from "vue";
import { api, errorText, type CredentialProvider, type CredentialRecord, type OperationResult } from "../admin";
import ErrorPanel from "../components/ErrorPanel.vue";
import PageHeader from "../components/PageHeader.vue";
import ResultPanel from "../components/ResultPanel.vue";
import StatusBadge from "../components/StatusBadge.vue";
const rows = ref<CredentialRecord[]>([]); const busy = ref(false); const error = ref(""); const result = ref<OperationResult | null>(null); const activeProvider = ref<CredentialProvider | null>(null);
async function load() { busy.value = true; error.value = ""; try { rows.value = await api.credentials(); } catch (e) { error.value = errorText(e); } finally { busy.value = false; } }
async function setCredential(provider: CredentialProvider) {
  busy.value = true; error.value = ""; result.value = null; activeProvider.value = provider;
  try { result.value = await api.credentialSet(provider); rows.value = await api.credentials(); }
  catch (e) { error.value = errorText(e); }
  finally { busy.value = false; activeProvider.value = null; }
}
function providerLabel(provider: CredentialProvider): string {
  return ({ anthropic: "Anthropic", openai: "OpenAI", deepseek: "DeepSeek", xai: "xAI", zai: "Z.ai", "ollama-cloud": "Ollama Cloud", huggingface: "Hugging Face" })[provider];
}
onMounted(load);
</script>
<template>
  <PageHeader title="Credentials" description="Configuration status only. Secret values never enter Tauri IPC, frontend state, logs or process arguments." :busy="busy"><button class="secondary" :disabled="busy" @click="load">Refresh</button></PageHeader>
  <ErrorPanel v-if="error" :message="error" />
  <ResultPanel v-if="result" :result="result" />
  <section class="credential-grid">
    <article v-for="row in rows" :key="row.provider" class="metric-card credential-card">
      <span class="card-label">{{ providerLabel(row.provider) }}</span>
      <StatusBadge :state="row.configured ? 'configured' : 'not configured'" />
      <button class="small secondary" :disabled="busy" @click="setCredential(row.provider)">{{ activeProvider === row.provider ? 'Terminal open…' : (row.configured ? 'Replace' : 'Set credential') }}</button>
    </article>
    <article class="security-card"><strong>Protected secret entry</strong><p>Set or replace opens a separate trusted GNOME terminal. The active release helper reads the secret invisibly from that terminal; the value never enters this webview or Tauri IPC. System authorization may ask for confirmation according to the active polkit policy.</p></article>
  </section>
</template>
