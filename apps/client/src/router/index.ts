import { createRouter, createWebHistory } from "vue-router";
import Home from "../views/Home.vue";
import Portfolio from "../views/Portfolio.vue";
import Status from "../views/Status.vue";

export const router = createRouter({
  history: createWebHistory(),
  routes: [
    { path: "/", name: "home", component: Home },
    { path: "/portfolio", name: "portfolio", component: Portfolio },
    { path: "/status", name: "status", component: Status },
  ],
});
