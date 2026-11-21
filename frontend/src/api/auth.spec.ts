import { afterEach, describe, expect, it, vi } from 'vitest'
import { fetchAuthStatus, fetchSyncState, isAuthStatusResponse } from './client'

// GET /api/auth/status応答の型ガードとauth client(TASK-9設計6章・8章・12章P4)。
// P4実装前のためfetchAuthStatus/isAuthStatusResponseはclient.tsに未実装=
// 本fileのtestは実行failを許容(実装後に緑化)。

afterEach(() => {
  vi.unstubAllGlobals()
})

// [covers:frontend.auth-status-client-and-store]
describe('auth statusの型ガードとclient', () => {
  it('GET /api/auth/status応答形式を判定する', () => {
    expect(isAuthStatusResponse({ authenticated: false, credentials: 0 })).toBe(true)
    expect(isAuthStatusResponse({ authenticated: true, credentials: 2 })).toBe(true)
    expect(isAuthStatusResponse({ authenticated: 'yes', credentials: 0 })).toBe(false)
    expect(isAuthStatusResponse({ credentials: 1 })).toBe(false)
    expect(isAuthStatusResponse({ authenticated: false, credentials: '1' })).toBe(false)
    expect(isAuthStatusResponse(null)).toBe(false)
  })

  it('fetchAuthStatusは200の認証状態を返す', async () => {
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
    const result = await fetchAuthStatus()
    expect(result.ok).toBe(true)
    if (result.ok) {
      expect(result.data.authenticated).toBe(false)
      expect(result.data.credentials).toBe(1)
    }
  })

  it('fetchAuthStatusはnetwork失敗をerror扱いにする', async () => {
    vi.stubGlobal(
      'fetch',
      vi.fn().mockRejectedValue(new TypeError('fetch failed')),
    )
    const result = await fetchAuthStatus()
    expect(result.ok).toBe(false)
    if (!result.ok) {
      expect(result.error.code).toBe('network')
      expect(result.httpStatus).toBe(0)
    }
  })
})

// [covers:frontend.unauthenticated-401-handling]
describe('保護APIの401(unauthenticated)の扱い', () => {
  it('401はerror扱い(code=unauthenticated・httpStatus=401)で503 configurationと区別される', async () => {
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
    // 保護APIの401観測(P4実装後の未認証応答。fetchSyncState=既存の保護API client)
    const result = await fetchSyncState()
    expect(result.ok).toBe(false)
    if (!result.ok) {
      expect(result.error.code).toBe('unauthenticated')
      expect(result.httpStatus).toBe(401)
      // P2〜P3の閉鎖(503 configuration)とはcodeで区別される(未認証時のUI誘導へ)
      expect(result.error.code).not.toBe('configuration')
    }
  })
})
