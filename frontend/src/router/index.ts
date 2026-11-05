import { createRouter, createWebHistory } from 'vue-router'
import HomeView from '../views/HomeView.vue'

// TASK-9設計7章: P2はindex的な1画面のみ。P4+でLoginView・EntitiesView等を追加。
export const router = createRouter({
  history: createWebHistory(),
  routes: [{ path: '/', name: 'home', component: HomeView }],
})
