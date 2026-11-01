<script setup lang="ts">
import { ref } from "vue";
import NavIcon, { type IconName } from "../NavIcon.vue";

// Tab strip with roving focus: arrow keys / Home / End move and select.
// "tabs" = the node page tab bar, "sub" = the sub-tabs inside a panel.
export type SegmentItem = { id: string; label: string; icon?: IconName };
const props = withDefaults(defineProps<{ items: SegmentItem[]; modelValue: string; label: string; variant?: "tabs" | "sub"; controls?: string }>(), {
  variant: "sub",
});
const emit = defineEmits<{ "update:modelValue": [id: string] }>();
const buttons = ref<HTMLButtonElement[]>([]);

function onKey(event: KeyboardEvent, index: number) {
  const last = props.items.length - 1;
  const next = { ArrowRight: index + 1, ArrowLeft: index - 1, Home: 0, End: last }[event.key];
  if (next === undefined) return;
  event.preventDefault();
  const target = (next + props.items.length) % props.items.length;
  emit("update:modelValue", props.items[target].id);
  buttons.value[target]?.focus();
}
</script>

<template>
  <div class="jv-seg seg" :class="variant" role="tablist" :aria-label="label">
    <button
      v-for="(item, i) in items"
      :key="item.id"
      ref="buttons"
      type="button"
      role="tab"
      :aria-selected="item.id === modelValue"
      :aria-controls="controls"
      :tabindex="item.id === modelValue ? 0 : -1"
      :class="{ on: item.id === modelValue }"
      @click="emit('update:modelValue', item.id)"
      @keydown="onKey($event, i)"
    >
      <NavIcon v-if="item.icon" :name="item.icon" />
      <span>{{ item.label }}</span>
    </button>
  </div>
</template>

<style scoped>
/* Look comes from the global .seg control; only sizes differ per variant. */
.jv-seg { display: flex; align-items: stretch; min-width: 0; }
button { flex: 1; min-width: 0; display: flex; align-items: center; justify-content: center; gap: 8px; white-space: nowrap; }
button :deep(svg) { flex: none; width: 18px; height: 18px; stroke-width: 1.6; }
button span { overflow: hidden; text-overflow: ellipsis; }
.tabs button { min-height: 38px; }

@media (max-width: 1099px), (max-height: 759px) {
  .jv-seg { overflow-x: auto; scrollbar-width: none; }
  .jv-seg button { flex: 1 0 auto; padding: 0 12px; }
  .tabs button { font-size: 13px; }
}
</style>
