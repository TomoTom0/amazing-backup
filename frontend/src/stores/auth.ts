import { defineStore } from 'pinia'
import { fetchAuthStatus, postApiKeyAuth, postLogout, type ApiError } from '../api/client'

/** 認証状態の取得状態(未取得=authUi unknown) */
export type AuthFetchState = 'idle' | 'loading' | 'ok' | 'error'

/** LoginViewのUI誘導状態(TASK-9設計7章・8章・12章P4) */
export type AuthUiState = 'unknown' | 'register' | 'login' | 'authenticated'

type AuthState = {
  fetchState: AuthFetchState
  authenticated: boolean
  credentialCount: number
  lastError: ApiError | null
  /** api-key認証の専用失敗状態(TASK-13設計2.4: passkey/bootstrap欄のlastErrorとは
   *  分離。error codeをそのまま保持=Viewで誤変換しない。寿命はlastErrorと同一で
   *  認証成功・fetchAuthStatus成功・logoutでクリア) */
  apiKeyError: ApiError | null
}

/**
 * 認証store(TASK-9設計7章: Pinia stores=auth・8章passkey認証・12章P4+TASK-13
 * api-key認証)。GET /api/auth/status(認証不要)で認証状態・登録credential数を取得し、
 * authUi getterがLoginViewのUI誘導(register/login/authenticated)を提供する
 * (frontend.auth-status-client-and-store・frontend.login-view-auth-ui-state)。
 */
export const useAuthStore = defineStore('auth', {
  state: (): AuthState => ({
    fetchState: 'idle',
    authenticated: false,
    credentialCount: 0,
    lastError: null,
    apiKeyError: null,
  }),
  getters: {
    /** LoginViewのUI誘導状態: 未取得=unknown・未認証+credentials=0はregister
     * (bootstrap token入力による登録案内)・未認証+credentials>0はlogin・認証済みはauthenticated */
    authUi(state): AuthUiState {
      if (state.fetchState !== 'ok') return 'unknown'
      if (state.authenticated) return 'authenticated'
      return state.credentialCount > 0 ? 'login' : 'register'
    },
    /** api-key欄の表示基盤(TASK-13設計2.4): 認証状態取得済み(fetchState=ok)かつ
     * 未認証時に表示。authUi(register/login)に依存しない=passkey 0件+api-keyのみ
     * のuserがlogin画面へ到達できない状態を作らない */
    showApiKeyField(state): boolean {
      return state.fetchState === 'ok' && !state.authenticated
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
        this.apiKeyError = null
        return
      }
      this.fetchState = 'error'
      this.authenticated = false
      this.credentialCount = 0
      this.lastError = result.error
    },
    /** api-key認証(TASK-13設計2.4: POST /api/auth/api-key。失敗はlastErrorとは分離
     *  したapiKeyErrorへcodeをそのまま保持。成功時は認証状態を再取得して
     *  authenticatedへ=passkey loginと同一遷移) */
    async authenticateWithApiKey(apiKey: string): Promise<void> {
      const result = await postApiKeyAuth(apiKey)
      if (!result.ok) {
        this.apiKeyError = result.error
        return
      }
      this.apiKeyError = null
      await this.fetchAuthStatus()
    },
    /** logout(POST /api/auth/logout。未認証401応答でもUI状態は未認証へ揃える。
     *  apiKeyErrorもクリア=未認証状態の失敗表示を持ち越さない) */
    async logout(): Promise<void> {
      const result = await postLogout()
      if (!result.ok) {
        this.lastError = result.error
      }
      this.apiKeyError = null
      this.authenticated = false
    },
  },
})
