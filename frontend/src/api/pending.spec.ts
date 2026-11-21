import { afterEach, describe, expect, it, vi } from 'vitest'
import { fetchPending, isPendingResponse, postPendingScan } from './client'

// GET /api/pending・POST /api/pending/scan応答の型ガードとpending client
// (TASK-9設計6章・7章・12章P5)。P5実装前のためfetchPending/isPendingResponse/
// postPendingScanはclient.tsに未実装=本fileのtestは実行failを許容(実装後に緑化)。
// PendingViewの実描画は実browser観測のため条件書frontend.tomlのexcluded参照。

afterEach(() => {
  vi.unstubAllGlobals()
})

// [covers:frontend.pending-client-and-store]
describe('GET /api/pending応答の型ガードとclient', () => {
  it('応答形式(pending配列+item schema)を判定する', () => {
    const item = {
      source_root: '/data',
      path: '/data/a.zip',
      size: 1024,
      mtime: '2026-11-05T10:00:00.000000+09:00',
      status: 'unbacked',
      last_scan_at: '2026-11-05T11:00:00.000000+09:00',
    }
    expect(isPendingResponse({ pending: [item] })).toBe(true)
    expect(isPendingResponse({ pending: [] })).toBe(true)
    // item fieldの型不一致(size・status等)はfalse
    expect(isPendingResponse({ pending: [{ ...item, size: '1024' }] })).toBe(false)
    expect(isPendingResponse({ pending: [{ ...item, status: 1 }] })).toBe(false)
    expect(isPendingResponse({ pending: [{ ...item, last_scan_at: null }] })).toBe(false)
    // pending配列欠落・形式違い・nullはfalse
    expect(isPendingResponse({ items: [item] })).toBe(false)
    expect(isPendingResponse({ pending: item })).toBe(false)
    expect(isPendingResponse(null)).toBe(false)
  })

  it('fetchPendingは200の一覧をdataへmappingする', async () => {
    vi.stubGlobal(
      'fetch',
      vi.fn().mockImplementation(() =>
        Promise.resolve(
          new Response(
            JSON.stringify({
              pending: [
                {
                  source_root: '/data',
                  path: '/data/a.zip',
                  size: 1024,
                  mtime: '2026-11-05T10:00:00.000000+09:00',
                  status: 'unbacked',
                  last_scan_at: '2026-11-05T11:00:00.000000+09:00',
                },
              ],
            }),
            { status: 200, headers: { 'Content-Type': 'application/json' } },
          ),
        ),
      ),
    )
    const result = await fetchPending()
    expect(result.ok).toBe(true)
    if (result.ok) {
      expect(result.data.pending.length).toBe(1)
      expect(result.data.pending[0].status).toBe('unbacked')
    }
  })

  it('fetchPendingは未認証401をerror扱いにする(保護API)', async () => {
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
    const result = await fetchPending()
    expect(result.ok).toBe(false)
    if (!result.ok) {
      expect(result.error.code).toBe('unauthenticated')
      expect(result.httpStatus).toBe(401)
    }
  })
})

// [covers:frontend.pending-scan-trigger]
describe('POST /api/pending/scan client', () => {
  it('scanはPOST /api/pending/scanへ応答形式を検証してmappingする', async () => {
    let capturedUrl = ''
    let capturedMethod = ''
    vi.stubGlobal(
      'fetch',
      vi.fn().mockImplementation((input: RequestInfo | URL, init?: RequestInit) => {
        capturedUrl = typeof input === 'string' ? input : input.toString()
        capturedMethod = init?.method ?? ''
        return Promise.resolve(
          new Response(JSON.stringify({ pending: [] }), {
            status: 200,
            headers: { 'Content-Type': 'application/json' },
          }),
        )
      }),
    )
    const result = await postPendingScan()
    expect(result.ok).toBe(true)
    if (result.ok) {
      expect(result.data.pending.length).toBe(0)
    }
    expect(capturedUrl).toBe('/api/pending/scan')
    expect(capturedMethod).toBe('POST')
  })
})
