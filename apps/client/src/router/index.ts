import { createRouter, createWebHistory } from "vue-router";
import Home from "../views/Home.vue";
import Trading from "../views/Trading.vue";
import Status from "../views/Status.vue";
import Settings from "../views/Settings.vue";

export const router = createRouter({
  history: createWebHistory(),
  routes: [
    // SYSTEM (Jarvis) world
    { path: "/", name: "home", component: Home },
    { path: "/status", name: "status", component: Status },
    { path: "/settings", name: "settings", component: Settings },

    // TRADING world — one component, sub-tab driven by the path
    { path: "/trading", name: "trading", component: Trading },
    { path: "/trading/ibkr", name: "trading-ibkr", component: Trading },

    // Legacy paths → new structure
    { path: "/portfolio", redirect: "/trading" },
    { path: "/broker", redirect: "/trading/ibkr" },
  ],
});
