import { reactive } from "vue";
import type { OperationStore } from "./release";

// Module scope, not view state: a lock or a page change unmounts the Releases
// view, but an install it started keeps running and its outcome lands here.
export const operations = reactive<OperationStore>({ current: null });
