<script setup lang="ts">
// Minimal line icons for the navigation (sidebar + mobile tab bar).
defineProps<{
  name:
    | "core"
    | "chart"
    | "link"
    | "pulse"
    | "gear"
    | "sound-on"
    | "sound-off"
    | "headset"
    | "mic"
    | "send";
}>();
</script>

<template>
  <svg
    viewBox="0 0 24 24"
    width="20"
    height="20"
    fill="none"
    stroke="currentColor"
    stroke-width="1.7"
    stroke-linecap="round"
    stroke-linejoin="round"
    aria-hidden="true"
  >
    <g v-if="name === 'core'">
      <path d="M12 3 21 12 12 21 3 12Z" />
      <circle cx="12" cy="12" r="2.4" fill="currentColor" stroke="none" />
    </g>
    <g v-else-if="name === 'chart'">
      <path d="M4 4v16h16" />
      <path d="M7 15l3-4 3 2 4-6" />
    </g>
    <g v-else-if="name === 'link'">
      <path d="M9.5 14.5 14.5 9.5" />
      <path d="M10 7l1-1a3.2 3.2 0 0 1 4.5 4.5l-1 1" />
      <path d="M14 17l-1 1a3.2 3.2 0 0 1-4.5-4.5l1-1" />
    </g>
    <g v-else-if="name === 'pulse'">
      <path d="M3 12h4l2.5-7 4 14 2.5-7H21" />
    </g>
    <g v-else-if="name === 'gear'">
      <circle cx="12" cy="12" r="3.2" />
      <path
        d="M12 2.6v2.2M12 19.2v2.2M21.4 12h-2.2M4.8 12H2.6M18.6 5.4l-1.6 1.6M7 17l-1.6 1.6M18.6 18.6 17 17M7 7 5.4 5.4"
      />
    </g>
    <g v-else-if="name === 'sound-on'">
      <path d="M4 9v6h4l5 4V5L8 9H4Z" />
      <path d="M16.5 8.5a5 5 0 0 1 0 7" />
      <path d="M19 6a8 8 0 0 1 0 12" />
    </g>
    <g v-else-if="name === 'sound-off'">
      <path d="M4 9v6h4l5 4V5L8 9H4Z" />
      <path d="M17 9.5l4 5M21 9.5l-4 5" />
    </g>
    <g v-else-if="name === 'headset'">
      <path d="M5 13v-1a7 7 0 0 1 14 0v1" />
      <rect x="3" y="13" width="3.6" height="6" rx="1.4" />
      <rect x="17.4" y="13" width="3.6" height="6" rx="1.4" />
    </g>
    <g v-else-if="name === 'mic'">
      <rect x="9" y="3" width="6" height="11" rx="3" />
      <path d="M5 11a7 7 0 0 0 14 0" />
      <path d="M12 18v3M8.5 21h7" />
    </g>
    <g v-else-if="name === 'send'">
      <path d="M22 2 11 13" />
      <path d="M22 2 15 22l-4-9-9-4 20-7Z" />
    </g>
  </svg>
</template>
