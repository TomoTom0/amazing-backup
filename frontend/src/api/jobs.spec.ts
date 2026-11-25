import { afterEach, describe, expect, it, vi } from 'vitest'
import {
  fetchJobDetail,
  fetchJobs,
  isJobResponse,
  isJobsResponse,
  postJobCancel,
  postJobsGet,
} from './client'

// jobs client(TASK-17設計2.1・2.3・3.2・9章W4=server.jobs-list/server.jobs-detail/
// server.jobs-get-issue/server.jobs-cancelの契約に整合)。job objectは
// {id,kind,status,params,progress,result,error,created_at,updated_at}のW4拡張schema
// (params=発行body・progress={phase,total,done,current_path}(waiting_restore中のみ
// next_check_at)・result=get成功時{name,dest}・error=失敗時{code,message})。

const jobItem = {
  id: 1,
  kind: 'get',
  status: 'waiting_restore',
  params: {
    uuid: '11111111-1111-1111-1111-111111111111',
    version_id: 'v1',
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
  it('形式一致のみtrue(W4拡張schema: params/progress/result/error込み)', () => {
    expect(isJobsResponse({ jobs: [jobItem] })).toBe(true)
    // jobが無ければ空配列
    expect(isJobsResponse({ jobs: [] })).toBe(true)
    // 成功job(result付き・error=null・paramsのnull許容)
    expect(
      isJobsResponse({
        jobs: [
          {
            ...jobItem,
            status: 'succeeded',
            params: { ...jobItem.params, version_id: null },
            progress: { phase: 'fetch', total: 1, done: 1, current_path: 'k' },
            result: { name: 'alpha', dest: 'alpha' },
          },
        ],
      }),
    ).toBe(true)
    // 失敗job(error付き)
    expect(
      isJobsResponse({
        jobs: [
          {
            ...jobItem,
            status: 'failed',
            error: { code: 'not_found', message: '対象が見つかりません' },
          },
        ],
      }),
    ).toBe(true)
    // skip付きresult
    expect(isJobResponse({ job: { ...jobItem, result: { name: 'a', dest: 'a', skip: true } } })).toBe(
      true,
    )
  })

  it('W4 schema違反はfalse', () => {
    // jobs欠落・jobs非配列・要素のfield欠落・id非number
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
            ...jobItem,
            params: { ...jobItem.params, version_id: 5 },
          },
        ],
      }),
    ).toBe(false)
    expect(
      isJobsResponse({
        jobs: [
          {
            ...jobItem,
            progress: { ...jobItem.progress, done: '0' },
          },
        ],
      }),
    ).toBe(false)
    // resultがnullで無い時にname/dest欠落
    expect(
      isJobsResponse({
        jobs: [{ ...jobItem, result: { name: 'a' } }],
      }),
    ).toBe(false)
    // errorのcode非string
    expect(
      isJobsResponse({
        jobs: [{ ...jobItem, error: { code: 1, message: 'x' } }],
      }),
    ).toBe(false)
    // paramsがnullで無い時に非object
    expect(
      isJobsResponse({
        jobs: [{ ...jobItem, params: 'x' }],
      }),
    ).toBe(false)
    expect(isJobsResponse(null)).toBe(false)
  })
})

// [covers:frontend.jobs-client-and-store]
describe('jobs client(fetchJobs)', () => {
  it('GET /api/jobsへ向かい(絞り込み無し)200応答のjobs一覧を返す', async () => {
    const calls: string[] = []
    vi.stubGlobal(
      'fetch',
      vi.fn().mockImplementation((input: RequestInfo | URL, init?: RequestInit) => {
        const url = typeof input === 'string' ? input : input.toString()
        calls.push(`${init?.method ?? 'GET'} ${url}`)
        return Promise.resolve(responseOf(200, { jobs: [jobItem] }))
      }),
    )
    const result = await fetchJobs()
    expect(calls).toEqual(['GET /api/jobs'])
    expect(result.ok).toBe(true)
    if (result.ok) {
      expect(result.data.jobs).toHaveLength(1)
      expect(result.data.jobs[0]?.progress?.phase).toBe('restore')
    }
  })

  it('status/kind絞り込みparamは値のあるもののみURL queryへ直列化する', async () => {
    const calls: string[] = []
    vi.stubGlobal(
      'fetch',
      vi.fn().mockImplementation((input: RequestInfo | URL, init?: RequestInit) => {
        const url = typeof input === 'string' ? input : input.toString()
        calls.push(`${init?.method ?? 'GET'} ${url}`)
        return Promise.resolve(responseOf(200, { jobs: [] }))
      }),
    )
    await fetchJobs({ status: 'running' })
    await fetchJobs({ kind: 'get' })
    await fetchJobs({ status: 'succeeded', kind: 'get' })
    await fetchJobs({})
    expect(calls).toEqual([
      'GET /api/jobs?status=running',
      'GET /api/jobs?kind=get',
      'GET /api/jobs?status=succeeded&kind=get',
      'GET /api/jobs',
    ])
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

// [covers:frontend.jobs-detail-and-cancel-client]
describe('jobs client(fetchJobDetail・postJobCancel)', () => {
  it('GET /api/jobs/:idへ向かい{"job":{...}}応答を返す・404はerror扱い', async () => {
    const calls: string[] = []
    vi.stubGlobal(
      'fetch',
      vi
        .fn()
        .mockImplementationOnce((input: RequestInfo | URL, init?: RequestInit) => {
          const url = typeof input === 'string' ? input : input.toString()
          calls.push(`${init?.method ?? 'GET'} ${url}`)
          return Promise.resolve(responseOf(200, { job: jobItem }))
        })
        .mockImplementationOnce(() =>
          Promise.resolve(
            responseOf(404, { error: { code: 'not_found', message: '対象がjobが見つかりません' } }),
          ),
        ),
    )
    const ok = await fetchJobDetail(3)
    expect(calls).toEqual(['GET /api/jobs/3'])
    expect(ok.ok).toBe(true)
    if (ok.ok) {
      expect(ok.data.job.id).toBe(1)
      expect(ok.data.job.params?.uuid).toBe('11111111-1111-1111-1111-111111111111')
    }
    const notFound = await fetchJobDetail(999)
    expect(notFound.ok).toBe(false)
    if (!notFound.ok) {
      expect(notFound.error.code).toBe('not_found')
      expect(notFound.httpStatus).toBe(404)
    }
  })

  it('POST /api/jobs/:id/cancelへ向かい200は取消後jobを・409はduplicate error', async () => {
    const calls: string[] = []
    vi.stubGlobal(
      'fetch',
      vi
        .fn()
        .mockImplementationOnce((input: RequestInfo | URL, init?: RequestInit) => {
          const url = typeof input === 'string' ? input : input.toString()
          calls.push(`${init?.method ?? 'GET'} ${url}`)
          return Promise.resolve(
            responseOf(200, { job: { ...jobItem, status: 'cancelled' } }),
          )
        })
        .mockImplementationOnce(() =>
          Promise.resolve(
            responseOf(409, {
              error: { code: 'duplicate', message: '実行中・完了済みのjobは取り消せません' },
            }),
          ),
        ),
    )
    const ok = await postJobCancel(5)
    expect(calls).toEqual(['POST /api/jobs/5/cancel'])
    expect(ok.ok).toBe(true)
    if (ok.ok) {
      expect(ok.data.job.status).toBe('cancelled')
    }
    const conflict = await postJobCancel(5)
    expect(conflict.ok).toBe(false)
    if (!conflict.ok) {
      expect(conflict.error.code).toBe('duplicate')
      expect(conflict.httpStatus).toBe(409)
    }
  })
})

// [covers:frontend.jobs-get-issue-client]
describe('jobs client(postJobsGet)', () => {
  it('POST /api/jobs/getへ発行bodyを送り200はqueuedのjob objectを返す', async () => {
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
        return Promise.resolve(responseOf(200, { job: jobItem }))
      }),
    )
    const result = await postJobsGet({
      uuid: '11111111-1111-1111-1111-111111111111',
      out: '/tmp/restore',
      tier: 'Standard',
      days: 7,
      overwrite: false,
    })
    expect(calls).toEqual(['POST /api/jobs/get'])
    expect(bodies).toEqual([
      {
        uuid: '11111111-1111-1111-1111-111111111111',
        out: '/tmp/restore',
        tier: 'Standard',
        days: 7,
        overwrite: false,
      },
    ])
    expect(result.ok).toBe(true)
    if (result.ok) {
      expect(result.data.job.status).toBe('waiting_restore')
    }
  })

  it('422 validation_failedはerror扱い', async () => {
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
    const result = await postJobsGet({
      uuid: 'u',
      out: '/tmp/x',
      tier: 'Standard',
      days: 0,
      overwrite: false,
    })
    expect(result.ok).toBe(false)
    if (!result.ok) {
      expect(result.error.code).toBe('validation_failed')
      expect(result.httpStatus).toBe(422)
    }
  })
})
