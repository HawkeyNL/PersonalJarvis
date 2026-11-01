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
const FONT = { family: "IBM Plex Sans", size: 12 };

// Chart.js paints on a canvas, which cannot read CSS variables; resolve each
// token to plain sRGB by painting one pixel (works for oklch() on every WebView).
const probe = document.createElement("canvas").getContext("2d", { willReadFrequently: true });
function token(name: string): string {
  const value = getComputedStyle(document.documentElement).getPropertyValue(name).trim();
  if (!probe || !value) return value;
  probe.clearRect(0, 0, 1, 1);
  probe.fillStyle = "#010203";
  probe.fillStyle = value;
  if (probe.fillStyle === "#010203") return value; // colour not parsed here; let Chart.js try it
  probe.fillRect(0, 0, 1, 1);
  const [r, g, b, a] = probe.getImageData(0, 0, 1, 1).data;
  return `rgba(${r}, ${g}, ${b}, ${(a / 255).toFixed(3)})`;
}
const euro = new Intl.NumberFormat(undefined, { style: "currency", currency: "EUR" });

function renderChart() {
  chart?.destroy();
  chart = null;
  if (!canvas.value || !props.rows.length) return;

  const rows = props.rows;
  const muted = token("--text-secondary");
  const text = token("--text-primary");
  // A surface-coloured edge between stacked segments, so they do not rely on hue alone.
  const edge = { borderColor: token("--bg-surface"), borderWidth: 1 };
  chart = new Chart(canvas.value, {
    type: "bar",
    data: {
      labels: rows.map((row) => row.day.slice(8)),
      datasets: [
        { label: "Input", data: rows.map((row) => row.input_tokens), backgroundColor: token("--chart-accent"), ...edge },
        { label: "Output", data: rows.map((row) => row.output_tokens), backgroundColor: token("--chart-ma"), ...edge },
        { label: "Cache read", data: rows.map((row) => row.cache_read_tokens), backgroundColor: token("--positive-solid"), ...edge },
        { label: "Cache write", data: rows.map((row) => row.cache_write_tokens), backgroundColor: token("--negative-solid"), ...edge },
      ],
    },
    options: {
      responsive: true,
      maintainAspectRatio: false,
      animation: { duration: matchMedia("(prefers-reduced-motion: reduce)").matches ? 0 : 240 },
      interaction: { intersect: false, mode: "index" },
      plugins: {
        legend: {
          position: "bottom",
          labels: {
            color: muted,
            boxWidth: 9,
            boxHeight: 9,
            padding: 14,
            usePointStyle: true,
            font: FONT,
          },
        },
        tooltip: {
          backgroundColor: token("--bg-raised"),
          borderColor: token("--border"),
          borderWidth: 1,
          titleColor: text,
          bodyColor: text,
          footerColor: text,
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
          ticks: { color: muted, maxTicksLimit: 16, font: FONT },
        },
        y: {
          stacked: true,
          beginAtZero: true,
          border: { display: false },
          grid: { color: token("--separator") },
          ticks: {
            color: muted,
            font: FONT,
            callback(value) { return Intl.NumberFormat(undefined, { notation: "compact" }).format(Number(value)); },
          },
        },
      },
    },
  });
}

// Repaint with the new token values when the theme flips.
const themeWatch = new MutationObserver(renderChart);
onMounted(() => {
  renderChart();
  void document.fonts.ready.then(renderChart); // redraw once IBM Plex has loaded
  themeWatch.observe(document.documentElement, { attributes: true, attributeFilter: ["data-theme"] });
});
watch(() => props.rows, renderChart, { deep: true });
onBeforeUnmount(() => {
  themeWatch.disconnect();
  chart?.destroy();
});
</script>

<template>
  <div class="chartjs-container">
    <canvas ref="canvas" role="img" aria-label="Stacked daily input, output and cache token usage" />
  </div>
</template>
