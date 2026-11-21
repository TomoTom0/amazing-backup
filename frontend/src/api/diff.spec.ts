import { afterEach, describe, expect, it, vi } from 'vitest'
import { fetchDiff, isDiffResponse, postDiffRun } from './client'

// GET /api/diff・POST /api/diff応答の型ガードとdiff client(TASK-9設計6章・
// 7章・12章P6=diff UI)。P6実装前のためfetchDiff/isDiffResponse/postDiffRunは
// client.tsに未実装=本fileのtestは実行failを許容(実装後に緑化)。
// 応答はcmd_diffのJSON語彙{performed_at,entries,files,missing,unexpected}
// (server.diff-endpointsのP3解釈に整合)。DiffViewの実描画は実browser観測のため
// 条件書frontend.tomlのexcluded参照(UI状態はstores/diff.spec.tsのdiffView getterで担保)。

afterEach(() => {
  vi.unstubAllGlobals()
})

const diffBody = {
  performed_at: '2026-11-05T12:00:00+09:00',
  entries: 2,
  files: [
    { uuid: '0192a4f0-0000-7000-8000-000000000001', title: 'a.zip' },
    { uuid: '0192a4f0-0000-7000-8000-000000000002', title: 'b.zip' },
  ],
  missing: ['data/0192a4f0-0000-7000-8000-000000000001/default.age'],
  unexpected: [],
}

// [covers:frontend.diff-client-and-store]
describe('GET /api/diff応答の型ガードとclient', () => {
  it('応答形式(performed_at/entries/files/missing/unexpected)を判定する', () => {
    expect(isDiffResponse(diffBody)).toBe(true)
    expect(isDiffResponse({ ...diffBody, files: [] })).toBe(true)
    expect(isDiffResponse({ ...diffBody, missing: [], unexpected: ['data/x/default.age'] })).toBe(
      true,
    )
    // fieldの型不一致(entries数値以外・files要素のtitle欠落・missing非配列)はfalse
    expect(isDiffResponse({ ...diffBody, entries: '2' })).toBe(false)
    expect(isDiffResponse({ ...diffBody, performed_at: 123 })).toBe(false)
    expect(isDiffResponse({ ...diffBody, files: [{ uuid: 'u1' }] })).toBe(false)
    expect(isDiffResponse({ ...diffBody, files: 'a' })).toBe(false)
    expect(isDiffResponse({ ...diffBody, missing: 'key' })).toBe(false)
    expect(isDiffResponse({ ...diffBody, unexpected: [1] })).toBe(false)
    // field欠落・nullはfalse
    expect(isDiffResponse({ performed_at: '2026-11-05T12:00:00+09:00' })).toBe(false)
    expect(isDiffResponse(null)).toBe(false)
  })

  it('fetchDiffは200のdiff結果をdataへmappingする', async () => {
    vi.stubGlobal(
      'fetch',
      vi.fn().mockImplementation(() =>
        Promise.resolve(
          new Response(JSON.stringify(diffBody), {
            status: 200,
            headers: { 'Content-Type': 'application/json' },
          }),
        ),
      ),
    )
    const result = await fetchDiff()
    expect(result.ok).toBe(true)
    if (result.ok) {
      expect(result.data.performed_at).toBe('2026-11-05T12:00:00+09:00')
      expect(result.data.entries).toBe(2)
      expect(result.data.missing.length).toBe(1)
    }
  })

  it('fetchDiffは未実行404 not_foundをerror扱いにする(未実行状態の接続点)', async () => {
    vi.stubGlobal(
      'fetch',
      vi.fn().mockImplementation(() =>
        Promise.resolve(
          new Response(
            JSON.stringify({
              error: { code: 'not_found', message: 'diff結果は未実行です' },
            }),
            { status: 404, headers: { 'Content-Type': 'application/json' } },
          ),
        ),
      ),
    )
    const result = await fetchDiff()
    expect(result.ok).toBe(false)
    if (!result.ok) {
      expect(result.error.code).toBe('not_found')
      expect(result.httpStatus).toBe(404)
    }
  })

  it('fetchDiffは未認証401をerror扱いにする(保護API)', async () => {
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
    const result = await fetchDiff()
    expect(result.ok).toBe(false)
    if (!result.ok) {
      expect(result.error.code).toBe('unauthenticated')
      expect(result.httpStatus).toBe(401)
    }
  })
})

// [covers:frontend.diff-run-trigger]
describe('POST /api/diff client', () => {
  it('diff実行はPOST /api/diffへ応答形式を検証してmappingする', async () => {
    let capturedUrl = ''
    let capturedMethod = ''
    vi.stubGlobal(
      'fetch',
      vi.fn().mockImplementation((input: RequestInfo | URL, init?: RequestInit) => {
        capturedUrl = typeof input === 'string' ? input : input.toString()
        capturedMethod = init?.method ?? ''
        return Promise.resolve(
          new Response(JSON.stringify(diffBody), {
            status: 200,
            headers: { 'Content-Type': 'application/json' },
          }),
        )
      }),
    )
    const result = await postDiffRun()
    expect(result.ok).toBe(true)
    if (result.ok) {
      expect(result.data.entries).toBe(2)
      expect(result.data.files.length).toBe(2)
    }
    expect(capturedUrl).toBe('/api/diff')
    expect(capturedMethod).toBe('POST')
  })
})
