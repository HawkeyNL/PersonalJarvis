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
  <div class="jv-seg" :class="variant" role="tablist" :aria-label="label">
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
.jv-seg { display: flex; align-items: stretch; min-width: 0; }
button {
  flex: 1; min-width: 0; display: flex; align-items: center; justify-content: center; gap: 12px;
  background: transparent; border: 1.5px solid transparent; color: var(--text-3);
  font: inherit; font-weight: 400; letter-spacing: 0.03em; cursor: pointer; white-space: nowrap;
}
button:hover { filter: none; color: var(--text-0); }
button :deep(svg) { flex: none; color: #9fb8ad; }
button.on { color: #f8fffb; font-weight: 500; border-color: var(--accent); }
button.on :deep(svg) { color: var(--accent); }
button span { overflow: hidden; text-overflow: ellipsis; }

.tabs {
  height: 56px; box-sizing: border-box; padding: 0 12px;
  border: 1px solid rgba(var(--accent-rgb), 0.35); border-bottom: none; border-radius: 30px 30px 0 0;
  background: rgba(4, 30, 24, 0.78);
}
.tabs button { height: 44px; align-self: center; border-radius: var(--r-14); font-size: 15px; }
.tabs button :deep(svg) { width: 22px; height: 22px; stroke-width: 1.6; }
.tabs button.on { background: rgba(var(--accent-rgb), 0.12); box-shadow: 0 0 18px rgba(var(--accent-rgb), 0.4), inset 0 0 12px rgba(var(--accent-rgb), 0.15); }

.sub {
  height: 40px; box-sizing: border-box; border: 1px solid var(--line-a30); border-radius: var(--r-12);
  background: rgba(3, 24, 19, 0.8);
}
.sub button { font-size: var(--fs-13); border-radius: var(--r-12); }
.sub button + button { border-left: 1px solid var(--line-a18); }
.sub button.on { background: rgba(var(--accent-rgb), 0.1); border: 1.5px solid var(--accent); box-shadow: 0 0 14px rgba(var(--accent-rgb), 0.35); }
.sub button :deep(svg) { width: 18px; height: 18px; }

@media (max-width: 1099px), (max-height: 759px) {
  .jv-seg { overflow-x: auto; scrollbar-width: none; }
  .tabs { border-radius: var(--r-22) var(--r-22) 0 0; padding: 0 8px; }
  button { flex: 1 0 auto; padding: 0 12px; gap: 8px; }
  .tabs button { font-size: 13px; }
}
</style>
