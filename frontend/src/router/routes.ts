import type { RouteRecordRaw } from 'vue-router'
import HomeView from '../views/HomeView.vue'
import LoginView from '../views/LoginView.vue'
import PendingView from '../views/PendingView.vue'
import DiffView from '../views/DiffView.vue'
import EntitiesView from '../views/EntitiesView.vue'
import EntityDetailView from '../views/EntityDetailView.vue'

// routes配列のみを切り出したmodule(TASK-9設計7章・コードレビュー指摘高4)。
// createWebHistoryはwindow依存のためnode環境(vitest)で動かない=router/index.tsから
// routes配列を分離することでvitestからのimport観測を可能にする観測seam。
export const routes: RouteRecordRaw[] = [
  { path: '/', name: 'home', component: HomeView },
  { path: '/login', name: 'login', component: LoginView },
  { path: '/pending', name: 'pending', component: PendingView },
  { path: '/diff', name: 'diff', component: DiffView },
  { path: '/entities', name: 'entities', component: EntitiesView },
  { path: '/entities/:uuid', name: 'entity-detail', component: EntityDetailView },
]
