import { afterEach, describe, expect, it, vi } from 'vitest'
import { postApiKeyAuth } from './client'

// api-key認証client(postApiKeyAuth=TASK-13設計D13・2.3/2.4章)。
// 実装前のためpostApiKeyAuthはclient.tsに未実装=本fileのtestは実行failを許容
// (実装後に緑化)。未実装参照は本file内に閉じる(他specは壊さない)。

afterEach(() => {
  vi.unstubAllGlobals()
})

// [covers:frontend.api-key-auth-client]
describe('api-key認証client(postApiKeyAuth)', () => {
  it('POST /api/auth/api-keyへ{"api_key"}bodyで呼び、200はok扱い', async () => {
    const fetchMock = vi
      .fn()
      .mockImplementation((_input: RequestInfo | URL, init?: RequestInit) => {
        expect(init?.method).toBe('POST')
        expect(String(init?.body)).toBe(JSON.stringify({ api_key: 'abk_test_key' }))
        return Promise.resolve(
          new Response(JSON.stringify({ status: 'ok' }), {
            status: 200,
            headers: { 'Content-Type': 'application/json' },
          }),
        )
      })
    vi.stubGlobal('fetch', fetchMock)
    const result = await postApiKeyAuth('abk_test_key')
    expect(result.ok).toBe(true)
    expect(fetchMock).toHaveBeenCalledTimes(1)
  })

  it('401 unauthenticatedはerror扱い(key不一致=LAN内の失敗)', async () => {
    vi.stubGlobal(
      'fetch',
      vi.fn().mockImplementation(() =>
        Promise.resolve(
          new Response(
            JSON.stringify({
              error: { code: 'unauthenticated', message: '認証に失敗しました' },
            }),
            { status: 401, headers: { 'Content-Type': 'application/json' } },
          ),
        ),
      ),
    )
    const result = await postApiKeyAuth('abk_wrong_key')
    expect(result.ok).toBe(false)
    if (!result.ok) {
      expect(result.error.code).toBe('unauthenticated')
      expect(result.httpStatus).toBe(401)
    }
  })

  it('403 forbidden_sourceはerror扱い(LAN外403とkey不一致401をcodeで区別)', async () => {
    vi.stubGlobal(
      'fetch',
      vi.fn().mockImplementation(() =>
        Promise.resolve(
          new Response(
            JSON.stringify({
              error: { code: 'forbidden_source', message: 'api-key認証はLAN内接続のみ有効です' },
            }),
            { status: 403, headers: { 'Content-Type': 'application/json' } },
          ),
        ),
      ),
    )
    const result = await postApiKeyAuth('abk_test_key')
    expect(result.ok).toBe(false)
    if (!result.ok) {
      expect(result.error.code).toBe('forbidden_source')
      expect(result.httpStatus).toBe(403)
    }
  })

  it('network失敗はcode=network・httpStatus=0', async () => {
    vi.stubGlobal(
      'fetch',
      vi.fn().mockRejectedValue(new TypeError('fetch failed')),
    )
    const result = await postApiKeyAuth('abk_test_key')
    expect(result.ok).toBe(false)
    if (!result.ok) {
      expect(result.error.code).toBe('network')
      expect(result.httpStatus).toBe(0)
    }
  })
})
