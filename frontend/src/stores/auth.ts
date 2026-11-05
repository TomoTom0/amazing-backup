import { defineStore } from 'pinia'
import { fetchAuthStatus, postLogout, type ApiError } from '../api/client'

/** 認証状態の取得状態(未取得=authUi unknown) */
export type AuthFetchState = 'idle' | 'loading' | 'ok' | 'error'

/** LoginViewのUI誘導状態(TASK-9設計7章・8章・12章P4) */
export type AuthUiState = 'unknown' | 'register' | 'login' | 'authenticated'

type AuthState = {
  fetchState: AuthFetchState
  authenticated: boolean
  credentialCount: number
  lastError: ApiError | null
}

/**
 * 認証store(TASK-9設計7章: Pinia stores=auth・8章passkey認証・12章P4)。
 * GET /api/auth/status(認証不要)で認証状態・登録credential数を取得し、
 * authUi getterがLoginViewのUI誘導(register/login/authenticated)を提供する
 * (frontend.auth-status-client-and-store・frontend.login-view-auth-ui-state)。
 */
export const useAuthStore = defineStore('auth', {
  state: (): AuthState => ({
    fetchState: 'idle',
    authenticated: false,
    credentialCount: 0,
    lastError: null,
  }),
  getters: {
    /** LoginViewのUI誘導状態: 未取得=unknown・未認証+credentials=0はregister
     * (bootstrap token入力による登録案内)・未認証+credentials>0はlogin・認証済みはauthenticated */
    authUi(state): AuthUiState {
      if (state.fetchState !== 'ok') return 'unknown'
      if (state.authenticated) return 'authenticated'
      return state.credentialCount > 0 ? 'login' : 'register'
    },
  },
  actions: {
    /** 認証状態・credential数の取得(GET /api/auth/status) */
    async fetchAuthStatus(): Promise<void> {
      this.fetchState = 'loading'
      const result = await fetchAuthStatus()
      if (result.ok) {
        this.fetchState = 'ok'
        this.authenticated = result.data.authenticated
        this.credentialCount = result.data.credentials
        this.lastError = null
        return
      }
      this.fetchState = 'error'
      this.authenticated = false
      this.credentialCount = 0
      this.lastError = result.error
    },
    /** logout(POST /api/auth/logout。未認証401応答でもUI状態は未認証へ揃える) */
    async logout(): Promise<void> {
      const result = await postLogout()
      if (!result.ok) {
        this.lastError = result.error
      }
      this.authenticated = false
    },
  },
})
