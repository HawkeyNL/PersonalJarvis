<script setup lang="ts">
import { computed } from "vue";
import { stateLabel, toneOf, type Tone } from "../status";
// The one status idiom: a badge whose label carries the meaning, coloured by tone.
// `state` is a raw CLI state (label and tone derive from it); `tone` and `label`
// override. Without any label only the dot renders (decoration, aria-hidden).
const props = defineProps<{ state?: string; tone?: Tone; label?: string }>();
const CLASSES: Record<Tone, string> = { ok: "pos", warn: "neg", error: "dng", idle: "" };
const text = computed(() => props.label ?? (props.state === undefined ? undefined : stateLabel(props.state)));
const toneClass = computed(() => CLASSES[props.tone ?? (props.state === undefined ? "idle" : toneOf(props.state))]);
</script>

<template>
  <span v-if="text" class="badge" :class="toneClass"><span class="dot" aria-hidden="true"></span>{{ text }}</span>
  <span v-else class="status-dot" :class="toneClass" aria-hidden="true"></span>
</template>
