<script setup lang="ts">
import {
  BarController,
  BarElement,
  CategoryScale,
  Chart,
  Legend,
  LinearScale,
  Tooltip,
} from "chart.js";
import { onBeforeUnmount, onMounted, ref, watch } from "vue";
import type { DailyUsageRow } from "../admin";

const props = defineProps<{ rows: DailyUsageRow[] }>();
const canvas = ref<HTMLCanvasElement | null>(null);
let chart: Chart<"bar", number[], string> | null = null;

Chart.register(BarController, BarElement, CategoryScale, LinearScale, Tooltip, Legend);

const integer = new Intl.NumberFormat();
const euro = new Intl.NumberFormat(undefined, { style: "currency", currency: "EUR" });

function renderChart() {
  chart?.destroy();
  chart = null;
  if (!canvas.value || !props.rows.length) return;

  const rows = props.rows;
  chart = new Chart(canvas.value, {
    type: "bar",
    data: {
      labels: rows.map((row) => row.day.slice(8)),
      datasets: [
        { label: "Input", data: rows.map((row) => row.input_tokens), backgroundColor: "#2ee6a2" },
        { label: "Output", data: rows.map((row) => row.output_tokens), backgroundColor: "#9bffd6" },
        { label: "Cache read", data: rows.map((row) => row.cache_read_tokens), backgroundColor: "#1f7a5a" },
        { label: "Cache write", data: rows.map((row) => row.cache_write_tokens), backgroundColor: "#f5b84a" },
      ],
    },
    options: {
      responsive: true,
      maintainAspectRatio: false,
      animation: { duration: 240 },
      interaction: { intersect: false, mode: "index" },
      plugins: {
        legend: {
          position: "bottom",
          labels: {
            color: "#8fb3a3",
            boxWidth: 9,
            boxHeight: 9,
            padding: 14,
            usePointStyle: true,
            font: { family: "Exo 2", size: 11 },
          },
        },
        tooltip: {
          backgroundColor: "#04201a",
          borderColor: "rgba(46, 230, 162, 0.3)",
          borderWidth: 1,
          titleColor: "#e6f4ee",
          bodyColor: "#e6f4ee",
          callbacks: {
            title(items) {
              const index = items[0]?.dataIndex;
              return index === undefined ? "" : rows[index]?.day ?? "";
            },
            label(context) {
              return `${context.dataset.label}: ${integer.format(context.parsed.y ?? 0)} tokens`;
            },
            footer(items) {
              const index = items[0]?.dataIndex;
              const row = index === undefined ? undefined : rows[index];
              return row ? `Total ${integer.format(row.total_tokens)} · ${euro.format(row.spent_eur)}` : "";
            },
          },
        },
      },
      scales: {
        x: {
          stacked: true,
          border: { display: false },
          grid: { display: false },
          ticks: { color: "#8fb3a3", maxTicksLimit: 16, font: { family: "Exo 2", size: 11 } },
        },
        y: {
          stacked: true,
          beginAtZero: true,
          border: { display: false },
          grid: { color: "rgba(46, 230, 162, 0.08)" },
          ticks: {
            color: "#8fb3a3",
            font: { family: "Exo 2", size: 11 },
            callback(value) { return Intl.NumberFormat(undefined, { notation: "compact" }).format(Number(value)); },
          },
        },
      },
    },
  });
}

onMounted(renderChart);
watch(() => props.rows, renderChart, { deep: true });
onBeforeUnmount(() => chart?.destroy());
</script>

<template>
  <div class="chartjs-container">
    <canvas ref="canvas" role="img" aria-label="Stacked daily input, output and cache token usage" />
  </div>
</template>
