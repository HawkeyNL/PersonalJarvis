<script setup lang="ts">
import { computed } from "vue";
import { toneOf } from "../admin";
import JvStatusDot from "./jv/JvStatusDot.vue";
const props = defineProps<{ state: string }>();
const tone = computed(() => toneOf(props.state));
</script>

<template><JvStatusDot class="status-badge" :tone="tone" :label="state.replace(/_/g, ' ')" :size="7" /></template>
