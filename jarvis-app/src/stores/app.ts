import { ref } from "vue";
import { defineStore } from "pinia";

/** Minimal app-wide store; grows as the client gains real state. */
export const useAppStore = defineStore("app", () => {
  const name = ref("Jarvis");
  const tagline = ref("Persoonlijk AI-besturingssysteem");
  return { name, tagline };
});
