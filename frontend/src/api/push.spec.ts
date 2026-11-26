import { afterEach, describe, expect, it, vi } from 'vitest'
import {
  isPushDryRunResponse,
  postJobsPush,
  postPushDryRun,
} from './client'

// TASK-17 W5(TASK-28): push dry-run(POST /api/push/dry-run)・push job発行
// (POST /api/jobs/push)のclientと応答型ガード(TASK-28設計D-P1/D-P2のschema)。

const dryRunBody = {
  connection: {
    profile: 'dev',
    bucket: 'api-bucket',
    prefix: 'amazing-backup/',
    recipients: ['SHA256:testfingerprint'],
  },
  targets: [
    {
      path: '/data/a.zip',
      name: 'a.zip',
      size: 100,
      action: 'skip',
      uuid: '11111111-1111-1111-1111-111111111111',
      title: 'alpha',
      warning: null,
    },
    {
      path: '/data/b.zip',
      name: 'b.zip',
      size: 200,
      action: 'new',
      uuid: null,
      title: null,
      warning: 'b.zip は新規発行予測ですがtitleが未指定です(実行時は対象エラーになります)',
    },
    // 同一run内の先行newへの合流merge: uuid=null+merge_target_index(先行対象のindex)
    {
      path: '/data/c.zip',
      name: 'c.zip',
      size: 200,
      action: 'merge',
      uuid: null,
      title: null,
      warning: null,
      merge_target_index: 1,
    },
    // 対象解決失敗の行: name/size=null+error{code,message}
    {
      path: '/data/d.zip',
      name: null,
      size: null,
      action: 'error',
      uuid: null,
      title: null,
      warning: null,
      error: {
        code: 'not_found',
        message: '対象エラー: 対象ファイルが存在しない: /data/d.zip',
      },
    },
  ],
  summary: { count: 4, total_size: 500, skip: 1, re_put: 0, merge: 1, new: 1, failed: 1 },
}

const queuedPushJob = {
  id: 10,
  kind: 'push',
  status: 'queued',
  params: {
    targets: [
      { path: '/data/a.zip', title: 'alpha' },
      { path: '/data/b.zip', title: null },
    ],
  },
  progress: {
    phase: 'queued',
    total: 2,
    done: 0,
    current_path: null,
  },
  result: null,
  error: null,
  created_at: '2026-11-25T10:00:00+09:00',
  updated_at: '2026-11-25T10:00:00+09:00',
}

const responseOf = (status: number, body: unknown): Response =>
  new Response(JSON.stringify(body), {
    status,
    headers: { 'Content-Type': 'application/json' },
  })

afterEach(() => {
  vi.unstubAllGlobals()
})

// [covers:frontend.push-dry-run-client]
describe('POST /api/push/dry-run応答の型ガードとclient(postPushDryRun)', () => {
  it('isPushDryRunResponseは形式一致のみtrue(connection/targets/summary)', () => {
    expect(isPushDryRunResponse(dryRunBody)).toBe(true)
    // re_put行・既知entityへのmerge(uuid付き)も受理する
    expect(
      isPushDryRunResponse({
        ...dryRunBody,
        targets: [
          {
            path: '/data/r.zip',
            name: 'r.zip',
            size: 1,
            action: 're_put',
            uuid: '22222222-2222-2222-2222-222222222222',
            title: 'reput',
            warning: null,
          },
          {
            path: '/data/m.zip',
            name: 'm.zip',
            size: 1,
            action: 'merge',
            uuid: '22222222-2222-2222-2222-222222222222',
            title: null,
            warning: null,
          },
        ],
        summary: {
          count: 2,
          total_size: 2,
          skip: 0,
          re_put: 1,
          merge: 1,
          new: 0,
          failed: 0,
        },
      }),
    ).toBe(true)
    expect(isPushDryRunResponse(null)).toBe(false)
    expect(isPushDryRunResponse({})).toBe(false)
    // 旧schema(items・total)は不一致
    expect(isPushDryRunResponse({ ...dryRunBody, targets: undefined })).toBe(false)
    expect(
      isPushDryRunResponse({
        ...dryRunBody,
        summary: { total: 4, total_size: 500, skip: 1, re_put: 0, merge: 1, new: 1 },
      }),
    ).toBe(false)
    // recipients非文字列
    expect(
      isPushDryRunResponse({
        ...dryRunBody,
        connection: { ...dryRunBody.connection, recipients: [123] },
      }),
    ).toBe(false)
    // action語彙違反(CLI表示語彙のre-putや失敗を表す語もAPI語彙でない)
    expect(
      isPushDryRunResponse({
        ...dryRunBody,
        targets: [{ ...dryRunBody.targets[0], action: 'bogus' }],
      }),
    ).toBe(false)
    expect(
      isPushDryRunResponse({
        ...dryRunBody,
        targets: [{ ...dryRunBody.targets[0], action: 're-put' }],
      }),
    ).toBe(false)
    // error行はerror{code,message}必須
    const errorRow = dryRunBody.targets[3]
    if (errorRow === undefined || errorRow.action !== 'error') throw new Error('test fixture')
    expect(
      isPushDryRunResponse({
        ...dryRunBody,
        targets: [{ ...errorRow, error: undefined }],
      }),
    ).toBe(false)
    expect(
      isPushDryRunResponse({
        ...dryRunBody,
        targets: [{ ...errorRow, error: { code: 'not_found', message: 1 } }],
      }),
    ).toBe(false)
    // merge_target_indexはnumber/null/省略のみ
    const mergeRow = dryRunBody.targets[2]
    if (mergeRow === undefined || mergeRow.action !== 'merge') throw new Error('test fixture')
    expect(
      isPushDryRunResponse({
        ...dryRunBody,
        targets: [{ ...mergeRow, merge_target_index: '1' }],
      }),
    ).toBe(false)
    expect(
      isPushDryRunResponse({
        ...dryRunBody,
        targets: [{ ...mergeRow, merge_target_index: null }],
      }),
    ).toBe(true)
    // summaryの型違反・欠落
    expect(
      isPushDryRunResponse({
        ...dryRunBody,
        summary: { ...dryRunBody.summary, count: '4' },
      }),
    ).toBe(false)
    expect(
      isPushDryRunResponse({ ...dryRunBody, summary: { total_size: 500, skip: 1 } }),
    ).toBe(false)
  })

  it('postPushDryRunはPOST /api/push/dry-runへtargetsを送り200/422/404を扱う', async () => {
    const calls: string[] = []
    const bodies: unknown[] = []
    vi.stubGlobal(
      'fetch',
      vi
        .fn()
        .mockImplementationOnce((input: RequestInfo | URL, init?: RequestInit) => {
          const url = typeof input === 'string' ? input : input.toString()
          calls.push(`${init?.method ?? 'GET'} ${url}`)
          bodies.push(JSON.parse(String(init?.body)))
          return Promise.resolve(responseOf(200, dryRunBody))
        })
        .mockImplementationOnce(() =>
          Promise.resolve(
            responseOf(404, { error: { code: 'not_found', message: '対象が見つかりません' } }),
          ),
        )
        .mockImplementationOnce(() =>
          Promise.resolve(
            responseOf(422, {
              error: { code: 'validation_failed', message: 'titleの形式が不正です' },
            }),
          ),
        ),
    )
    // 発行と同一schema(title未指定は省略)
    const ok = await postPushDryRun({
      targets: [{ path: '/data/a.zip', title: 'alpha' }, { path: '/data/b.zip' }],
    })
    expect(calls).toEqual(['POST /api/push/dry-run'])
    expect(bodies).toEqual([
      { targets: [{ path: '/data/a.zip', title: 'alpha' }, { path: '/data/b.zip' }] },
    ])
    expect(ok.ok).toBe(true)
    if (ok.ok) {
      expect(ok.data.connection.profile).toBe('dev')
      expect(ok.data.connection.recipients).toEqual(['SHA256:testfingerprint'])
      expect(ok.data.summary.new).toBe(1)
      expect(ok.data.summary.failed).toBe(1)
    }

    const notFound = await postPushDryRun({ targets: [{ path: '/data/absent.zip' }] })
    expect(notFound.ok).toBe(false)
    if (!notFound.ok) {
      expect(notFound.error.code).toBe('not_found')
      expect(notFound.httpStatus).toBe(404)
    }

    const invalid = await postPushDryRun({ targets: [{ path: '/data/a.zip', title: 'a/b' }] })
    expect(invalid.ok).toBe(false)
    if (!invalid.ok) {
      expect(invalid.error.code).toBe('validation_failed')
      expect(invalid.httpStatus).toBe(422)
    }
  })
})

// [covers:frontend.jobs-push-issue-client]
describe('POST /api/jobs/push client(postJobsPush)', () => {
  it('POST /api/jobs/pushへtargetsを送りqueuedのpush job objectを返す・422はerror扱い', async () => {
    const calls: string[] = []
    const bodies: unknown[] = []
    vi.stubGlobal(
      'fetch',
      vi
        .fn()
        .mockImplementationOnce((input: RequestInfo | URL, init?: RequestInit) => {
          const url = typeof input === 'string' ? input : input.toString()
          calls.push(`${init?.method ?? 'GET'} ${url}`)
          bodies.push(JSON.parse(String(init?.body)))
          return Promise.resolve(responseOf(200, { job: queuedPushJob }))
        })
        .mockImplementationOnce(() =>
          Promise.resolve(
            responseOf(422, {
              error: { code: 'validation_failed', message: 'pathはserver側の絶対pathで指定してください' },
            }),
          ),
        ),
    )
    // dry-runと同一のbody(title未指定は省略。応答paramsではnullでechoされる)
    const ok = await postJobsPush({
      targets: [{ path: '/data/a.zip', title: 'alpha' }, { path: '/data/b.zip' }],
    })
    expect(calls).toEqual(['POST /api/jobs/push'])
    expect(bodies).toEqual([
      { targets: [{ path: '/data/a.zip', title: 'alpha' }, { path: '/data/b.zip' }] },
    ])
    expect(ok.ok).toBe(true)
    if (ok.ok) {
      expect(ok.data.job.kind).toBe('push')
      expect(ok.data.job.status).toBe('queued')
      if (ok.data.job.kind === 'push') {
        expect(ok.data.job.params?.targets).toEqual([
          { path: '/data/a.zip', title: 'alpha' },
          { path: '/data/b.zip', title: null },
        ])
        expect(ok.data.job.progress?.phase).toBe('queued')
      }
    }

    const err = await postJobsPush({ targets: [{ path: 'relative/a.zip' }] })
    expect(err.ok).toBe(false)
    if (!err.ok) {
      expect(err.error.code).toBe('validation_failed')
      expect(err.httpStatus).toBe(422)
    }
  })
})
