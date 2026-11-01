<script setup lang="ts">
import { ref, watch } from "vue";
import { THEMES, currentTheme, setTheme, type ThemeChoice } from "../theme";

// Compact System / Light / Dark control for the top bar. A display
// preference only; it never reaches the broker.
const choice = ref<ThemeChoice>(currentTheme());
watch(choice, (value) => setTheme(value));
</script>

<template>
  <select v-model="choice" class="theme-select" aria-label="Colour theme">
    <option v-for="item in THEMES" :key="item.id" :value="item.id">{{ item.label }} theme</option>
  </select>
</template>
