import type { RouteRecordRaw } from 'vue-router'
import HomeView from '../views/HomeView.vue'
import LoginView from '../views/LoginView.vue'
import EntitiesView from '../views/EntitiesView.vue'
import EntityDetailView from '../views/EntityDetailView.vue'
import TransferView from '../views/TransferView.vue'
import SettingsView from '../views/SettingsView.vue'
import AdminView from '../views/AdminView.vue'

// routes配列のみを切り出したmodule(TASK-9設計7章・コードレビュー指摘高4)。
// createWebHistoryはwindow依存のためnode環境(vitest)で動かない=router/index.tsから
// routes配列を分離することでvitestからのimport観測を可能にする観測seam。
// TASK-17設計2.0・2.2・9章W1: 5タブ(/・/backups・/transfer・/settings・/admin)へ
// 再編。既存4画面は再配置(一覧/詳細→/backups配下・pending/diff→/backupsのfilter
// view(?filter=pending:<status>・?filter=diffのquery param)のため独立routeなし。
// /transfer・/settings・/adminはW1ではroute登録の枠(中身はW4〜W7)。
export const routes: RouteRecordRaw[] = [
  { path: '/', name: 'home', component: HomeView },
  { path: '/login', name: 'login', component: LoginView },
  { path: '/backups', name: 'backups', component: EntitiesView },
  { path: '/backups/:uuid', name: 'entity-detail', component: EntityDetailView },
  { path: '/transfer', name: 'transfer', component: TransferView },
  { path: '/settings', name: 'settings', component: SettingsView },
  { path: '/admin', name: 'admin', component: AdminView },
]
