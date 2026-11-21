import { afterEach, describe, expect, it, vi } from 'vitest'
import { fetchRuns, isRunsResponse } from './client'

// GET /api/runs応答の型ガードとruns client(TASK-17設計3.2・2.1・9章W1=ホームの
// 前回backup表示基盤。応答{runs:[{batch_id,date,entity_count}]}はserver.runs-list
// の解釈に整合)。実装前のためclient.tsへruns系は未実装=本fileのtestは実行failを
// 許容(実装後に緑化)。

const runsBody = {
  runs: [
    { batch_id: '20261106-abcdef02', date: '2026-11-06', entity_count: 2 },
    { batch_id: '20261105-abcdef01', date: '2026-11-05', entity_count: 1 },
  ],
}

const responseOf = (status: number, body: unknown): Response =>
  new Response(JSON.stringify(body), {
    status,
    headers: { 'Content-Type': 'application/json' },
  })

afterEach(() => {
  vi.unstubAllGlobals()
})

// [covers:frontend.runs-client-and-store]
describe('GET /api/runs応答の型ガード(isRunsResponse)', () => {
  it('形式一致のみtrue(runs配列+各要素のbatch_id/date/entity_count)', () => {
    expect(isRunsResponse(runsBody)).toBe(true)
    expect(isRunsResponse({ runs: [] })).toBe(true)
    // runs欠落・runs非配列・要素のfield欠落・entity_count非number・null
    expect(isRunsResponse({})).toBe(false)
    expect(isRunsResponse({ runs: 'x' })).toBe(false)
    expect(isRunsResponse({ runs: [{ batch_id: 'b', date: '2026-11-06' }] })).toBe(false)
    expect(
      isRunsResponse({ runs: [{ batch_id: 'b', date: '2026-11-06', entity_count: '2' }] }),
    ).toBe(false)
    expect(isRunsResponse(null)).toBe(false)
  })
})

// [covers:frontend.runs-client-and-store]
describe('runs client(fetchRuns)', () => {
  it('GET /api/runsへ向かい200応答のruns一覧を返す', async () => {
    const calls: string[] = []
    vi.stubGlobal(
      'fetch',
      vi.fn().mockImplementation((input: RequestInfo | URL, init?: RequestInit) => {
        const url = typeof input === 'string' ? input : input.toString()
        calls.push(`${init?.method ?? 'GET'} ${url}`)
        return Promise.resolve(responseOf(200, runsBody))
      }),
    )
    const result = await fetchRuns()
    expect(calls).toEqual(['GET /api/runs'])
    expect(result.ok).toBe(true)
    if (result.ok) {
      expect(result.data.runs).toHaveLength(2)
      expect(result.data.runs[0]?.batch_id).toBe('20261106-abcdef02')
      expect(result.data.runs[0]?.entity_count).toBe(2)
    }
  })

  it('401 unauthenticatedはerror扱い', async () => {
    vi.stubGlobal(
      'fetch',
      vi.fn().mockImplementation(() =>
        Promise.resolve(
          responseOf(401, { error: { code: 'unauthenticated', message: '認証が必要です' } }),
        ),
      ),
    )
    const result = await fetchRuns()
    expect(result.ok).toBe(false)
    if (!result.ok) {
      expect(result.error.code).toBe('unauthenticated')
      expect(result.httpStatus).toBe(401)
    }
  })
})
