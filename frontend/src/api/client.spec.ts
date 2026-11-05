import { afterEach, describe, expect, it, vi } from 'vitest'
import { apiGet, isApiErrorResponse, isHealthResponse } from './client'

// vitest setup(TASK-9 P2の型ガード+P3読取API waveのapiGet mapping。TASK-9設計6章・11章)

// [covers:frontend.api-error-body-guard]
describe('api型ガード', () => {
  it('error body形式を判定する', () => {
    expect(
      isApiErrorResponse({ error: { code: 'configuration', message: '閉鎖中' } }),
    ).toBe(true)
    expect(isApiErrorResponse({ error: { code: 1, message: 'x' } })).toBe(false)
    expect(isApiErrorResponse({ error: { code: 'a' } })).toBe(false)
    expect(isApiErrorResponse({})).toBe(false)
    expect(isApiErrorResponse(null)).toBe(false)
  })

  it('health応答形式を判定する', () => {
    expect(isHealthResponse({ status: 'ok' })).toBe(true)
    expect(isHealthResponse({ status: 1 })).toBe(false)
    expect(isHealthResponse('ok')).toBe(false)
  })
})

// fetch mockの後始末(各test後にglobal fetchを復元)
afterEach(() => {
  vi.unstubAllGlobals()
})

// [covers:frontend.api-get-result-mapping]
// [covers:frontend.fail-closed-protected-api-handling]
describe('apiGetの結果mapping', () => {
  it('成功(2xx+ガード通過)はok:trueのdataを返す', async () => {
    vi.stubGlobal(
      'fetch',
      vi.fn().mockResolvedValue(
        new Response(JSON.stringify({ status: 'ok' }), {
          status: 200,
          headers: { 'Content-Type': 'application/json' },
        }),
      ),
    )
    const result = await apiGet('/api/health', isHealthResponse)
    expect(result).toEqual({ ok: true, data: { status: 'ok' } })
  })

  it('非2xxのerror bodyはerror(code+message)とhttpStatusへmappingする', async () => {
    // 保護APIが未認証でfail-closed(503 configuration)のP2〜P3段階の代表観測
    vi.stubGlobal(
      'fetch',
      vi.fn().mockResolvedValue(
        new Response(
          JSON.stringify({
            error: { code: 'configuration', message: '保護APIは認証機構の構成が完了するまで閉鎖されています' },
          }),
          { status: 503, headers: { 'Content-Type': 'application/json' } },
        ),
      ),
    )
    const result = await apiGet('/api/entities', isHealthResponse)
    expect(result.ok).toBe(false)
    if (!result.ok) {
      expect(result.error.code).toBe('configuration')
      expect(result.error.message).not.toBe('')
      expect(result.httpStatus).toBe(503)
    }
  })

  it('network失敗はcode=network・httpStatus=0へmappingする', async () => {
    vi.stubGlobal(
      'fetch',
      vi.fn().mockRejectedValue(new TypeError('fetch failed')),
    )
    const result = await apiGet('/api/health', isHealthResponse)
    expect(result).toEqual({
      ok: false,
      error: { code: 'network', message: 'serverに接続できません' },
      httpStatus: 0,
    })
  })

  it('2xxでも型ガード不一致はcode=internal扱いにする', async () => {
    vi.stubGlobal(
      'fetch',
      vi.fn().mockResolvedValue(
        new Response(JSON.stringify({ unexpected: true }), {
          status: 200,
          headers: { 'Content-Type': 'application/json' },
        }),
      ),
    )
    const result = await apiGet('/api/health', isHealthResponse)
    expect(result.ok).toBe(false)
    if (!result.ok) {
      expect(result.error.code).toBe('internal')
      expect(result.httpStatus).toBe(200)
    }
  })
})
