import { createRouter, createWebHistory } from 'vue-router'
import { routes } from './routes'
import { useAuthStore } from '../stores/auth'

// TASK-9設計7章: P4はLoginView追加+未認証時誘導(router guard軽量版=P4条件の範囲。
// P5はPendingView追加・P6はDiffView追加。entities画面(EntitiesView・
// EntityDetailView)はコードレビュー指摘高4で追加)。routes配列はroutes.tsへ分離。
export const router = createRouter({
  history: createWebHistory(),
  routes,
})

// 未認証時はLoginViewへ誘導(認証状態はGET /api/auth/status由来。保護API自体は
// server側が401で閉じるためこのguardはUI誘導のみ)
router.beforeEach(async (to) => {
  const auth = useAuthStore()
  if (to.name === 'login') {
    return true
  }
  if (auth.fetchState !== 'ok') {
    await auth.fetchAuthStatus()
  }
  if (!auth.authenticated) {
    return { name: 'login' }
  }
  return true
})
