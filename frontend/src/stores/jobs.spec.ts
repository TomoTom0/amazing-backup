import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { createPinia, setActivePinia } from 'pinia'
import { useJobsStore } from './jobs'
import { useUiRefreshStore } from './ui-refresh'
import type { JobItem } from '../api/client'

// jobs store(TASK-17設計2.1・2.3・5.3・9章W4=server.jobs系条件の契約に整合):
// job一覧(W4拡張schema)・activeJobs getter・取得jobの発行(issueGet/rerunGet)・
// 取消(cancelJob)・詳細(fetchJobDetail)・polling(設計5.3: ui-refreshの間隔・
// 参照カウント式start/stop)。

/** W4 schemaのjob item fixture(進行中=waiting_restore) */
const waitingJob: JobItem = {
  id: 1,
  kind: 'get',
  status: 'waiting_restore',
  params: {
    uuid: '11111111-1111-1111-1111-111111111111',
    version_id: null,
    pin: null,
    out: '/tmp/restore',
    tier: 'Standard',
    days: 7,
    overwrite: false,
  },
  progress: {
    phase: 'restore',
    total: 1,
    done: 0,
    current_path: 'amazing-backup/data/1111/default.age',
    next_check_at: '2026-11-24T10:15:00+09:00',
  },
  result: null,
  error: null,
  created_at: '2026-11-24T10:00:00+09:00',
  updated_at: '2026-11-24T10:05:00+09:00',
}

const doneJob: JobItem = {
  ...waitingJob,
  id: 2,
  status: 'succeeded',
  progress: { phase: 'fetch', total: 1, done: 1, current_path: 'k' },
  result: { name: 'alpha', dest: 'alpha' },
}

const failedJob: JobItem = {
  ...waitingJob,
  id: 3,
  status: 'failed',
  error: { code: 'not_found', message: '対象が見つかりません' },
}

const responseOf = (status: number, body: unknown): Response =>
  new Response(JSON.stringify(body), {
    status,
    headers: { 'Content-Type': 'application/json' },
  })

/** localStorage相当のin-memory実装(vi.stubGlobal注入用の観測seam。
 *  ui-refresh storeのsetIntervalMsがlocalStorageへ書くためpolling testで必要) */
class MemoryStorage implements Storage {
  private readonly map = new Map<string, string>()
  get length(): number {
    return this.map.size
  }
  key(index: number): string | null {
    return Array.from(this.map.keys())[index] ?? null
  }
  getItem(key: string): string | null {
    return this.map.get(key) ?? null
  }
  setItem(key: string, value: string): void {
    this.map.set(key, value)
  }
  removeItem(key: string): void {
    this.map.delete(key)
  }
  clear(): void {
    this.map.clear()
  }
}

beforeEach(() => {
  setActivePinia(createPinia())
  vi.stubGlobal('localStorage', new MemoryStorage())
})

afterEach(() => {
  vi.useRealTimers()
  vi.unstubAllGlobals()
})

// [covers:frontend.jobs-client-and-store]
describe('jobs store(fetchJobs・activeJobs)', () => {
  it('成功時はW4拡張schemaのjobs一覧を保持する', async () => {
    vi.stubGlobal(
      'fetch',
      vi.fn().mockImplementation(() =>
        Promise.resolve(responseOf(200, { jobs: [waitingJob, doneJob, failedJob] })),
      ),
    )
    const jobs = useJobsStore()
    await jobs.fetchJobs()
    expect(jobs.items).toHaveLength(3)
    expect(jobs.items[0]?.progress?.next_check_at).toBe('2026-11-24T10:15:00+09:00')
    expect(jobs.items[2]?.error?.code).toBe('not_found')
    expect(jobs.lastError).toBeNull()
  })

  it('activeJobsは進行中(queued/running/waiting_restore)のみ。hasActiveJobsの判定', async () => {
    vi.stubGlobal(
      'fetch',
      vi.fn().mockImplementation(() =>
        Promise.resolve(responseOf(200, { jobs: [waitingJob, doneJob, failedJob] })),
      ),
    )
    const jobs = useJobsStore()
    await jobs.fetchJobs()
    expect(jobs.activeJobs).toHaveLength(1)
    expect(jobs.activeJobs[0]?.id).toBe(1)
    expect(jobs.hasActiveJobs).toBe(true)
    // 完了分のみなら非表示扱い
    vi.stubGlobal(
      'fetch',
      vi.fn().mockImplementation(() =>
        Promise.resolve(responseOf(200, { jobs: [doneJob, failedJob] })),
      ),
    )
    await jobs.fetchJobs()
    expect(jobs.activeJobs).toHaveLength(0)
    expect(jobs.hasActiveJobs).toBe(false)
  })

  it('失敗時はlastErrorを保持し保持済みitemsは空へ戻す(空dataを表示しない)', async () => {
    const jobsBody = { jobs: [waitingJob] }
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
})

// [covers:frontend.jobs-get-issue-client]
describe('jobs store(issueGet・rerunGet)', () => {
  it('issueGetはPOST /api/jobs/get後に一覧を再取得する(issuingの遷移)', async () => {
    const calls: string[] = []
    const bodies: unknown[] = []
    vi.stubGlobal(
      'fetch',
      vi.fn().mockImplementation((input: RequestInfo | URL, init?: RequestInit) => {
        const url = typeof input === 'string' ? input : input.toString()
        calls.push(`${init?.method ?? 'GET'} ${url}`)
        const parsed: unknown =
          init?.body === undefined || init?.body === null ? null : JSON.parse(String(init.body))
        bodies.push(parsed)
        if (init?.method === 'POST') {
          return Promise.resolve(responseOf(200, { job: waitingJob }))
        }
        return Promise.resolve(responseOf(200, { jobs: [waitingJob] }))
      }),
    )
    const jobs = useJobsStore()
    const pending = jobs.issueGet({
      uuid: '11111111-1111-1111-1111-111111111111',
      out: '/tmp/restore',
      tier: 'Standard',
      days: 7,
      overwrite: false,
    })
    expect(jobs.issuing).toBe(true)
    const ok = await pending
    expect(ok).toBe(true)
    expect(jobs.issuing).toBe(false)
    expect(jobs.lastIssueError).toBeNull()
    expect(calls).toEqual(['POST /api/jobs/get', 'GET /api/jobs'])
    expect(bodies[0]).toEqual({
      uuid: '11111111-1111-1111-1111-111111111111',
      out: '/tmp/restore',
      tier: 'Standard',
      days: 7,
      overwrite: false,
    })
    expect(jobs.items).toHaveLength(1)
  })

  it('issueGet失敗(422)はlastIssueErrorで保持し一覧は変更しない', async () => {
    const jobs = useJobsStore()
    vi.stubGlobal(
      'fetch',
      vi.fn().mockImplementation(() =>
        Promise.resolve(
          responseOf(422, {
            error: { code: 'validation_failed', message: 'daysは1〜365で指定してください' },
          }),
        ),
      ),
    )
    const ok = await jobs.issueGet({
      uuid: 'u',
      out: '/tmp/x',
      tier: 'Standard',
      days: 0,
      overwrite: false,
    })
    expect(ok).toBe(false)
    expect(jobs.issuing).toBe(false)
    expect(jobs.lastIssueError?.code).toBe('validation_failed')
    expect(jobs.items).toEqual([])
    expect(jobs.lastError).toBeNull()
  })

  it('rerunGetはjobのparamsで同一bodyを再発行する(冪等な再実行)', async () => {
    const calls: string[] = []
    const bodies: unknown[] = []
    vi.stubGlobal(
      'fetch',
      vi.fn().mockImplementation((input: RequestInfo | URL, init?: RequestInit) => {
        const url = typeof input === 'string' ? input : input.toString()
        calls.push(`${init?.method ?? 'GET'} ${url}`)
        const parsed: unknown =
          init?.body === undefined || init?.body === null ? null : JSON.parse(String(init.body))
        bodies.push(parsed)
        if (init?.method === 'POST') {
          return Promise.resolve(responseOf(200, { job: waitingJob }))
        }
        return Promise.resolve(responseOf(200, { jobs: [doneJob] }))
      }),
    )
    const jobs = useJobsStore()
    const ok = await jobs.rerunGet(doneJob)
    expect(ok).toBe(true)
    expect(calls).toEqual(['POST /api/jobs/get', 'GET /api/jobs'])
    // 再発行bodyは元jobのparams(nullのversion_id/pinは省略=server側でnull扱い)
    expect(bodies[0]).toEqual({
      uuid: '11111111-1111-1111-1111-111111111111',
      out: '/tmp/restore',
      tier: 'Standard',
      days: 7,
      overwrite: false,
    })
    // paramsが読み取れないjobはinternal errorで発行しない
    const noParams: JobItem = { ...doneJob, params: null }
    const ok2 = await jobs.rerunGet(noParams)
    expect(ok2).toBe(false)
    expect(jobs.lastIssueError?.code).toBe('internal')
  })
})

// [covers:frontend.jobs-detail-and-cancel-client]
describe('jobs store(fetchJobDetail・cancelJob)', () => {
  it('fetchJobDetailは詳細を保持し・404はdetailError', async () => {
    vi.stubGlobal(
      'fetch',
      vi
        .fn()
        .mockImplementationOnce(() => Promise.resolve(responseOf(200, { job: waitingJob })))
        .mockImplementationOnce(() =>
          Promise.resolve(
            responseOf(404, { error: { code: 'not_found', message: '対象のjobが見つかりません' } }),
          ),
        ),
    )
    const jobs = useJobsStore()
    await jobs.fetchJobDetail(1)
    expect(jobs.detail?.id).toBe(1)
    expect(jobs.detailError).toBeNull()
    await jobs.fetchJobDetail(999)
    expect(jobs.detail).toBeNull()
    expect(jobs.detailError?.code).toBe('not_found')
  })

  it('cancelJobはPOST cancel後に一覧を再取得する・409はlastCancelError', async () => {
    const calls: string[] = []
    vi.stubGlobal(
      'fetch',
      vi.fn().mockImplementation((input: RequestInfo | URL, init?: RequestInit) => {
        const url = typeof input === 'string' ? input : input.toString()
        calls.push(`${init?.method ?? 'GET'} ${url}`)
        if (url === '/api/jobs/1/cancel') {
          return Promise.resolve(responseOf(200, { job: { ...waitingJob, status: 'cancelled' } }))
        }
        return Promise.resolve(responseOf(200, { jobs: [] }))
      }),
    )
    const jobs = useJobsStore()
    await jobs.cancelJob(1)
    expect(jobs.lastCancelError).toBeNull()
    expect(calls).toEqual(['POST /api/jobs/1/cancel', 'GET /api/jobs'])
    // 409(実行中)はlastCancelError・一覧再取得無し
    calls.length = 0
    vi.stubGlobal(
      'fetch',
      vi.fn().mockImplementation((input: RequestInfo | URL, init?: RequestInit) => {
        const url = typeof input === 'string' ? input : input.toString()
        calls.push(`${init?.method ?? 'GET'} ${url}`)
        return Promise.resolve(
          responseOf(409, {
            error: { code: 'duplicate', message: '実行中・完了済みのjobは取り消せません' },
          }),
        )
      }),
    )
    await jobs.cancelJob(2)
    expect(jobs.lastCancelError?.code).toBe('duplicate')
    expect(calls).toEqual(['POST /api/jobs/2/cancel'])
  })
})

// [covers:frontend.jobs-polling]
describe('jobs store(polling)', () => {
  it('startPollingはui-refreshのintervalMs間隔でfetchJobsを繰り返す', async () => {
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
    jobs.startPolling()
    // 既定10秒間隔
    await vi.advanceTimersByTimeAsync(30_000)
    expect(calls).toHaveLength(4)
    // stopPollingで停止(残り参照が0)
    jobs.stopPolling()
    await vi.advanceTimersByTimeAsync(30_000)
    expect(calls).toHaveLength(4)
  })

  it('参照カウント: 複数のstartPollingは独立にstopできる(nav dot用と表示中追従)', async () => {
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
    jobs.startPolling()
    jobs.startPolling()
    await vi.advanceTimersByTimeAsync(10_000)
    const countAfterBoth = calls.length
    expect(countAfterBoth).toBeGreaterThanOrEqual(1)
    // 片方停止ではpolling継続
    jobs.stopPolling()
    await vi.advanceTimersByTimeAsync(10_000)
    expect(calls.length).toBeGreaterThan(countAfterBoth)
    // 全停止で停止
    jobs.stopPolling()
    const countAtStop = calls.length
    await vi.advanceTimersByTimeAsync(20_000)
    expect(calls.length).toBe(countAtStop)
  })

  it('intervalMs=null(自動更新無効)時はfetchせず・再有効化で次tickから反映', async () => {
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
    const uiRefresh = useUiRefreshStore()
    uiRefresh.setIntervalMs(null)
    const jobs = useJobsStore()
    jobs.startPolling()
    await vi.advanceTimersByTimeAsync(30_000)
    expect(calls).toHaveLength(0)
    // 5000msへ再有効化→次tickから5秒間隔でfetch
    uiRefresh.setIntervalMs(5000)
    await vi.advanceTimersByTimeAsync(10_000)
    expect(calls.length).toBe(2)
  })

  it('間隔変更は次tickから反映される(毎tick読み直し)', async () => {
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
    const uiRefresh = useUiRefreshStore()
    uiRefresh.setIntervalMs(10_000)
    const jobs = useJobsStore()
    jobs.startPolling()
    await vi.advanceTimersByTimeAsync(10_000)
    expect(calls).toHaveLength(1)
    uiRefresh.setIntervalMs(5000)
    // 進行中の待機timer(t=10s発火分)は旧間隔のままで・その次のtickから5秒間隔に
    // 反映される: 15秒進めるとt=20(旧間隔timer)・t=25(新間隔)の2fetchが加わる
    await vi.advanceTimersByTimeAsync(15_000)
    expect(calls).toHaveLength(3)
  })
})
