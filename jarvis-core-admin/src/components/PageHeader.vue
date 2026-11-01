<script setup lang="ts">
import { inject, watch } from "vue";
const props = defineProps<{ title: string; description: string; busy?: boolean }>();
// A view embedded in another page (Health and Services on Overview) gets an h2.
const level = inject("pageHeadingLevel", 1);
// While busy every action stays in place but disabled (the fieldset); the
// button whose click started the work shows the spinner via aria-busy. A click
// only counts if busy starts in the same task, so a later busy state that a
// row action or timer starts marks no header button.
let clicked: HTMLButtonElement | null = null;
let marked: HTMLButtonElement | null = null;
function onClick(event: MouseEvent) {
  clicked = (event.target as Element).closest("button");
  setTimeout(() => { clicked = null; });
}
watch(() => props.busy, (busy) => {
  marked?.removeAttribute("aria-busy");
  marked = busy ? clicked : null;
  marked?.setAttribute("aria-busy", "true");
});
</script>

<template>
  <header class="page-header">
    <div>
      <h1 v-if="level === 1">{{ title }}</h1>
      <h2 v-else>{{ title }}</h2>
      <p>{{ description }}</p>
    </div>
    <fieldset class="page-actions" :disabled="busy" @click.capture="onClick"><slot /></fieldset>
  </header>
</template>
