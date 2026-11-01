import { createApp } from "vue";
import App from "./App.vue";
import { initTheme } from "./theme";
import "./tokens.css";
import "./styles.css";

initTheme();
createApp(App).mount("#app");
