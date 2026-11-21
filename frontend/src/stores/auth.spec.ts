import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { createPinia, setActivePinia } from 'pinia'
import { useAuthStore } from './auth'

// auth storeの認証状態管理(TASK-9設計6章・7章・8章・12章P4)。P4実装前のため
// src/stores/auth.tsは未実装=本fileのtestは実行failを許容(実装後に緑化)。
// LoginViewのUI誘導状態は本storeのauthUi getterで担保する(component実描画・
// WebAuthn API呼出は実browser観測のため条件書frontend.tomlのexcluded参照)。

beforeEach(() => {
  setActivePinia(createPinia())
})

afterEach(() => {
  vi.unstubAllGlobals()
})

// [covers:frontend.auth-status-client-and-store]
describe('auth storeのfetchAuthStatus', () => {
  it('成功時は認証状態・credential数を保持する', async () => {
    vi.stubGlobal(
      'fetch',
      vi.fn().mockImplementation(() =>
        Promise.resolve(
          new Response(JSON.stringify({ authenticated: false, credentials: 1 }), {
            status: 200,
            headers: { 'Content-Type': 'application/json' },
          }),
        ),
      ),
    )
    const auth = useAuthStore()
    await auth.fetchAuthStatus()
    expect(auth.authenticated).toBe(false)
    expect(auth.credentialCount).toBe(1)
  })

  it('失敗時はlastErrorを保持する', async () => {
    vi.stubGlobal(
      'fetch',
      vi.fn().mockRejectedValue(new TypeError('fetch failed')),
    )
    const auth = useAuthStore()
    await auth.fetchAuthStatus()
    expect(auth.lastError).not.toBeNull()
  })
})

// [covers:frontend.auth-logout-store]
describe('auth storeのlogout', () => {
  it('logout成功で未認証へ遷移する', async () => {
    // 認証済み状態を作ってからlogoutする(auth/statusは認証済み・logoutは200)
    vi.stubGlobal(
      'fetch',
      vi.fn().mockImplementation((input: RequestInfo | URL) => {
        const url = typeof input === 'string' ? input : input.toString()
        if (url === '/api/auth/logout') {
          return Promise.resolve(
            new Response('{}', {
              status: 200,
              headers: { 'Content-Type': 'application/json' },
            }),
          )
        }
        return Promise.resolve(
          new Response(JSON.stringify({ authenticated: true, credentials: 1 }), {
            status: 200,
            headers: { 'Content-Type': 'application/json' },
          }),
        )
      }),
    )
    const auth = useAuthStore()
    await auth.fetchAuthStatus()
    expect(auth.authenticated).toBe(true)
    await auth.logout()
    expect(auth.authenticated).toBe(false)
  })

  it('未認証401応答のlogoutでもUI状態は未認証へ揃う', async () => {
    vi.stubGlobal(
      'fetch',
      vi.fn().mockImplementation(() =>
        Promise.resolve(
          new Response(
            JSON.stringify({
              error: { code: 'unauthenticated', message: '認証が必要です' },
            }),
            { status: 401, headers: { 'Content-Type': 'application/json' } },
          ),
        ),
      ),
    )
    const auth = useAuthStore()
    await auth.logout()
    expect(auth.authenticated).toBe(false)
  })
})

// [covers:frontend.login-view-auth-ui-state]
describe('authUi getter(LoginViewのUI誘導状態)', () => {
  it('未取得はunknown', () => {
    const auth = useAuthStore()
    expect(auth.authUi).toBe('unknown')
  })

  it('未認証+credentials=0はregister(bootstrap token入力による登録案内)', async () => {
    vi.stubGlobal(
      'fetch',
      vi.fn().mockImplementation(() =>
        Promise.resolve(
          new Response(JSON.stringify({ authenticated: false, credentials: 0 }), {
            status: 200,
            headers: { 'Content-Type': 'application/json' },
          }),
        ),
      ),
    )
    const auth = useAuthStore()
    await auth.fetchAuthStatus()
    expect(auth.authUi).toBe('register')
  })

  it('未認証+credentials>0はlogin(passkey登録済み状態表示)', async () => {
    vi.stubGlobal(
      'fetch',
      vi.fn().mockImplementation(() =>
        Promise.resolve(
          new Response(JSON.stringify({ authenticated: false, credentials: 2 }), {
            status: 200,
            headers: { 'Content-Type': 'application/json' },
          }),
        ),
      ),
    )
    const auth = useAuthStore()
    await auth.fetchAuthStatus()
    expect(auth.authUi).toBe('login')
  })

  it('認証済みはauthenticated', async () => {
    vi.stubGlobal(
      'fetch',
      vi.fn().mockImplementation(() =>
        Promise.resolve(
          new Response(JSON.stringify({ authenticated: true, credentials: 2 }), {
            status: 200,
            headers: { 'Content-Type': 'application/json' },
          }),
        ),
      ),
    )
    const auth = useAuthStore()
    await auth.fetchAuthStatus()
    expect(auth.authUi).toBe('authenticated')
  })
})
