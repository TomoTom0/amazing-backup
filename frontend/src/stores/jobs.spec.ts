import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { createPinia, setActivePinia } from 'pinia'
import { useJobsStore } from './jobs'

// jobs store(TASK-17設計2.1・5.3・9章W1): ホーム実行中job区画・転送タブのW1基盤。
// W1のserver応答は常に空配列(server.jobs-list)。W1ではpollingを行わずfetchJobsは
// 手動呼出のみ(pollingの実行はW4の転送タブ実装で追加=設計5.3)。実装前のため
// src/stores/jobs.tsは未実装=本fileのtestは実行failを許容(実装後に緑化)。

const responseOf = (status: number, body: unknown): Response =>
  new Response(JSON.stringify(body), {
    status,
    headers: { 'Content-Type': 'application/json' },
  })

beforeEach(() => {
  setActivePinia(createPinia())
})

afterEach(() => {
  vi.useRealTimers()
  vi.unstubAllGlobals()
})

// [covers:frontend.jobs-client-and-store]
describe('jobs store(fetchJobs)', () => {
  it('成功時はjobs一覧(W1は空)を保持する', async () => {
    vi.stubGlobal(
      'fetch',
      vi.fn().mockImplementation(() => Promise.resolve(responseOf(200, { jobs: [] }))),
    )
    const jobs = useJobsStore()
    await jobs.fetchJobs()
    expect(jobs.items).toEqual([])
    expect(jobs.lastError).toBeNull()
  })

  it('失敗時はlastErrorを保持し保持済みitemsは空へ戻す(空dataを表示しない)', async () => {
    // 非空一覧(W1のserver応答は常に空だがstoreはschemaどおり非空も保持する)を
    // 保持した状態での再取得失敗=保持済みitemsの消去も観測する(空の初期state
    // からの失敗だけではitemsを空へ戻す実装を被覆できない)
    const jobsBody = {
      jobs: [
        {
          id: 1,
          kind: 'get',
          status: 'running',
          created_at: '2026-11-14T10:00:00+09:00',
          updated_at: '2026-11-14T10:00:00+09:00',
        },
      ],
    }
    vi.stubGlobal(
      'fetch',
      vi
        .fn()
        .mockImplementationOnce(() => Promise.resolve(responseOf(200, jobsBody)))
        .mockImplementationOnce(() =>
          Promise.resolve(
            responseOf(401, { error: { code: 'unauthenticated', message: '認証が必要です' } }),
          ),
        ),
    )
    const jobs = useJobsStore()
    await jobs.fetchJobs()
    expect(jobs.items).toHaveLength(1)
    expect(jobs.lastError).toBeNull()
    await jobs.fetchJobs()
    expect(jobs.items).toEqual([])
    expect(jobs.lastError?.code).toBe('unauthenticated')
  })

  it('W1ではpollingを行わない(fetch後timerを進めてもfetchは増えない)', async () => {
    vi.useFakeTimers()
    const calls: string[] = []
    vi.stubGlobal(
      'fetch',
      vi.fn().mockImplementation((input: RequestInfo | URL, init?: RequestInit) => {
        const url = typeof input === 'string' ? input : input.toString()
        calls.push(`${init?.method ?? 'GET'} ${url}`)
        return Promise.resolve(responseOf(200, { jobs: [] }))
      }),
    )
    const jobs = useJobsStore()
    await jobs.fetchJobs()
    expect(calls).toEqual(['GET /api/jobs'])
    // 既定間隔10秒を3回分進めても自動再取得は起きない(pollingはW4=手動表示のみ)
    await vi.advanceTimersByTimeAsync(30_000)
    expect(calls).toEqual(['GET /api/jobs'])
  })
})
