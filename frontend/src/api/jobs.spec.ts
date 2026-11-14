import { afterEach, describe, expect, it, vi } from 'vitest'
import { fetchJobs, isJobsResponse } from './client'

// GET /api/jobs応答の型ガードとjobs client(TASK-17設計2.1・3.2・9章W1=ホーム実行中
// job区画・転送タブのW1基盤。W1のserver応答は常に空配列=server.jobs-list。要素は
// {id,kind,status,created_at,updated_at}の最小schemaで検証し進行(params/progress/
// result/error)の詳細fieldはW4のjob実装時に拡張)。実装前のためclient.tsへjobs系は
// 未実装=本fileのtestは実行failを許容(実装後に緑化)。

const jobItem = {
  id: 1,
  kind: 'get',
  status: 'running',
  created_at: '2026-11-14T10:00:00+09:00',
  updated_at: '2026-11-14T10:05:00+09:00',
}

const responseOf = (status: number, body: unknown): Response =>
  new Response(JSON.stringify(body), {
    status,
    headers: { 'Content-Type': 'application/json' },
  })

afterEach(() => {
  vi.unstubAllGlobals()
})

// [covers:frontend.jobs-client-and-store]
describe('GET /api/jobs応答の型ガード(isJobsResponse)', () => {
  it('形式一致のみtrue(jobs配列+要素のid/kind/status/created_at/updated_at)', () => {
    expect(isJobsResponse({ jobs: [jobItem] })).toBe(true)
    // W1のserver応答は常に空配列
    expect(isJobsResponse({ jobs: [] })).toBe(true)
    // jobs欠落・jobs非配列・要素のfield欠落・id非number・status非string・null
    expect(isJobsResponse({})).toBe(false)
    expect(isJobsResponse({ jobs: 'x' })).toBe(false)
    expect(
      isJobsResponse({
        jobs: [{ kind: 'get', status: 'running', created_at: 'c', updated_at: 'u' }],
      }),
    ).toBe(false)
    expect(
      isJobsResponse({
        jobs: [
          {
            id: '1',
            kind: 'get',
            status: 'running',
            created_at: 'c',
            updated_at: 'u',
          },
        ],
      }),
    ).toBe(false)
    expect(
      isJobsResponse({
        jobs: [{ id: 1, kind: 2, status: 'running', created_at: 'c', updated_at: 'u' }],
      }),
    ).toBe(false)
    expect(isJobsResponse(null)).toBe(false)
  })
})

// [covers:frontend.jobs-client-and-store]
describe('jobs client(fetchJobs)', () => {
  it('GET /api/jobsへ向かい200応答のjobs一覧を返す', async () => {
    const calls: string[] = []
    vi.stubGlobal(
      'fetch',
      vi.fn().mockImplementation((input: RequestInfo | URL, init?: RequestInit) => {
        const url = typeof input === 'string' ? input : input.toString()
        calls.push(`${init?.method ?? 'GET'} ${url}`)
        return Promise.resolve(responseOf(200, { jobs: [] }))
      }),
    )
    const result = await fetchJobs()
    expect(calls).toEqual(['GET /api/jobs'])
    expect(result.ok).toBe(true)
    if (result.ok) {
      expect(result.data.jobs).toEqual([])
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
    const result = await fetchJobs()
    expect(result.ok).toBe(false)
    if (!result.ok) {
      expect(result.error.code).toBe('unauthenticated')
      expect(result.httpStatus).toBe(401)
    }
  })
})
