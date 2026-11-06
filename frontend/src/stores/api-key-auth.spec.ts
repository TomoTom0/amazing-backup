import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { createPinia, setActivePinia } from 'pinia'
import { useAuthStore } from './auth'

// api-key認証のstore層(TASK-13設計2.4: LoginView api-key欄の表示基盤=authUiに
// 依存しない未認証時常時表示・api-key認証action・専用失敗状態apiKeyError)。
// 実装前のためauth storeのapi-key系(getter・action・apiKeyError)は未実装=
// 本fileのtestは実行failを許容(実装後に緑化)。未実装参照は本file内に閉じる
// (component実描画は実browser観測のためfrontend.tomlのexcluded参照)。

beforeEach(() => {
  setActivePinia(createPinia())
})

afterEach(() => {
  vi.unstubAllGlobals()
})

const statusResponse = (authenticated: boolean, credentials: number): Response =>
  new Response(JSON.stringify({ authenticated, credentials }), {
    status: 200,
    headers: { 'Content-Type': 'application/json' },
  })

const errorResponse = (status: number, code: string): Response =>
  new Response(
    JSON.stringify({ error: { code, message: 'message' } }),
    { status, headers: { 'Content-Type': 'application/json' } },
  )

// [covers:frontend.login-view-api-key-field]
describe('api-key欄の表示状態(authUi状態に依存しない常時表示)', () => {
  it('未取得(unknown)は非表示', () => {
    const auth = useAuthStore()
    expect(auth.authUi).toBe('unknown')
    expect(auth.showApiKeyField).toBe(false)
  })

  it('未認証+credentials=0(register状態)でも表示', async () => {
    vi.stubGlobal(
      'fetch',
      vi.fn().mockImplementation(() => Promise.resolve(statusResponse(false, 0))),
    )
    const auth = useAuthStore()
    await auth.fetchAuthStatus()
    expect(auth.authUi).toBe('register')
    expect(auth.showApiKeyField).toBe(true)
  })

  it('未認証+credentials>0(login状態)でも表示', async () => {
    vi.stubGlobal(
      'fetch',
      vi.fn().mockImplementation(() => Promise.resolve(statusResponse(false, 2))),
    )
    const auth = useAuthStore()
    await auth.fetchAuthStatus()
    expect(auth.authUi).toBe('login')
    expect(auth.showApiKeyField).toBe(true)
  })

  it('認証済みは非表示', async () => {
    vi.stubGlobal(
      'fetch',
      vi.fn().mockImplementation(() => Promise.resolve(statusResponse(true, 2))),
    )
    const auth = useAuthStore()
    await auth.fetchAuthStatus()
    expect(auth.authUi).toBe('authenticated')
    expect(auth.showApiKeyField).toBe(false)
  })
})

// [covers:frontend.login-view-api-key-field]
describe('api-key認証実行(authenticateWithApiKey)', () => {
  it('成功時は認証状態を再取得してauthenticatedへ(passkey loginと同一遷移)', async () => {
    const calls: string[] = []
    vi.stubGlobal(
      'fetch',
      vi.fn().mockImplementation((input: RequestInfo | URL) => {
        const url = typeof input === 'string' ? input : input.toString()
        calls.push(url)
        if (url === '/api/auth/api-key') {
          return Promise.resolve(
            new Response(JSON.stringify({ status: 'ok' }), {
              status: 200,
              headers: { 'Content-Type': 'application/json' },
            }),
          )
        }
        return Promise.resolve(statusResponse(true, 1))
      }),
    )
    const auth = useAuthStore()
    await auth.authenticateWithApiKey('abk_test_key')
    expect(calls).toEqual(['/api/auth/api-key', '/api/auth/status'])
    expect(auth.authenticated).toBe(true)
  })
})

// [covers:frontend.api-key-failure-dedicated-state]
describe('api-key認証の専用失敗状態(apiKeyError)', () => {
  it('401はapiKeyErrorへ(codeを変換せず保持・passkey側lastErrorとは分離)', async () => {
    vi.stubGlobal(
      'fetch',
      vi.fn().mockImplementation(() => Promise.resolve(errorResponse(401, 'unauthenticated'))),
    )
    const auth = useAuthStore()
    await auth.authenticateWithApiKey('abk_wrong_key')
    // codeをそのまま保持(401をbootstrap再発行案内へ誤変換しない)
    expect(auth.apiKeyError?.code).toBe('unauthenticated')
    // passkey/bootstrap欄の失敗(lastError)とは分離した専用状態
    expect(auth.lastError).toBeNull()
    expect(auth.authenticated).toBe(false)
  })

  it('403 forbidden_sourceもapiKeyErrorへ', async () => {
    vi.stubGlobal(
      'fetch',
      vi.fn().mockImplementation(() => Promise.resolve(errorResponse(403, 'forbidden_source'))),
    )
    const auth = useAuthStore()
    await auth.authenticateWithApiKey('abk_test_key')
    expect(auth.apiKeyError?.code).toBe('forbidden_source')
    expect(auth.lastError).toBeNull()
  })

  it('network失敗はcode=networkをapiKeyErrorへ', async () => {
    vi.stubGlobal(
      'fetch',
      vi.fn().mockRejectedValue(new TypeError('fetch failed')),
    )
    const auth = useAuthStore()
    await auth.authenticateWithApiKey('abk_test_key')
    expect(auth.apiKeyError?.code).toBe('network')
    expect(auth.lastError).toBeNull()
  })

  it('認証成功でapiKeyErrorはクリアされる', async () => {
    let fail = true
    vi.stubGlobal(
      'fetch',
      vi.fn().mockImplementation((input: RequestInfo | URL) => {
        const url = typeof input === 'string' ? input : input.toString()
        if (url === '/api/auth/api-key') {
          if (fail) {
            return Promise.resolve(errorResponse(401, 'unauthenticated'))
          }
          return Promise.resolve(
            new Response(JSON.stringify({ status: 'ok' }), {
              status: 200,
              headers: { 'Content-Type': 'application/json' },
            }),
          )
        }
        return Promise.resolve(statusResponse(true, 1))
      }),
    )
    const auth = useAuthStore()
    await auth.authenticateWithApiKey('abk_test_key')
    expect(auth.apiKeyError?.code).toBe('unauthenticated')
    fail = false
    await auth.authenticateWithApiKey('abk_test_key')
    expect(auth.apiKeyError).toBeNull()
    expect(auth.authenticated).toBe(true)
  })

  it('fetchAuthStatus成功でapiKeyErrorはクリアされる(lastErrorと同一寿命)', async () => {
    vi.stubGlobal(
      'fetch',
      vi.fn().mockImplementation((input: RequestInfo | URL) => {
        const url = typeof input === 'string' ? input : input.toString()
        if (url === '/api/auth/api-key') {
          return Promise.resolve(errorResponse(401, 'unauthenticated'))
        }
        return Promise.resolve(statusResponse(false, 0))
      }),
    )
    const auth = useAuthStore()
    await auth.authenticateWithApiKey('abk_wrong_key')
    expect(auth.apiKeyError?.code).toBe('unauthenticated')
    // 認証状態の再取得が成功すれば前回のapi-key失敗は残さない(lastErrorと同一寿命)
    await auth.fetchAuthStatus()
    expect(auth.apiKeyError).toBeNull()
  })

  it('logoutでapiKeyErrorはクリアされる(未認証状態へ失敗表示を持ち越さない)', async () => {
    vi.stubGlobal(
      'fetch',
      vi.fn().mockImplementation((input: RequestInfo | URL) => {
        const url = typeof input === 'string' ? input : input.toString()
        if (url === '/api/auth/api-key') {
          return Promise.resolve(errorResponse(401, 'unauthenticated'))
        }
        if (url === '/api/auth/logout') {
          return Promise.resolve(
            new Response(JSON.stringify({ status: 'ok' }), {
              status: 200,
              headers: { 'Content-Type': 'application/json' },
            }),
          )
        }
        return Promise.resolve(statusResponse(true, 1))
      }),
    )
    const auth = useAuthStore()
    await auth.authenticateWithApiKey('abk_wrong_key')
    expect(auth.apiKeyError?.code).toBe('unauthenticated')
    await auth.logout()
    expect(auth.apiKeyError).toBeNull()
    expect(auth.authenticated).toBe(false)
  })
})
