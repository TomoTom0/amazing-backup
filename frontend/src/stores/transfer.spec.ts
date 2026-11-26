import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { createPinia, setActivePinia } from 'pinia'
import { useTransferStore } from './transfer'
import { useJobsStore } from './jobs'

// 転送タブの取得form状態(TASK-17設計2.3・9章W4): form初期値(既定はCLI getと同一=
// tier Standard・days 7・overwrite false)・route queryからのprefill
// (?get=<uuid>&version_id=...・同一対象は再適用しない/別対象は再適用)・submitGet(jobs storeのissueGetへ
// 接続=POST /api/jobs/get後の一覧再取得。uuid/outのclient側最低限検証)。

const responseOf = (status: number, body: unknown): Response =>
  new Response(JSON.stringify(body), {
    status,
    headers: { 'Content-Type': 'application/json' },
  })

const queuedJob = {
  id: 1,
  kind: 'get',
  status: 'queued',
  params: {
    uuid: '11111111-1111-1111-1111-111111111111',
    version_id: null,
    pin: null,
    out: '/tmp/restore',
    tier: 'Standard',
    days: 7,
    overwrite: false,
  },
  progress: { phase: 'queued', total: 1, done: 0, current_path: null },
  result: null,
  error: null,
  created_at: '2026-11-24T10:00:00+09:00',
  updated_at: '2026-11-24T10:00:00+09:00',
}

beforeEach(() => {
  setActivePinia(createPinia())
})

afterEach(() => {
  vi.unstubAllGlobals()
})

// [covers:frontend.transfer-get-form]
describe('transfer store(form初期値・prefill)', () => {
  it('初期値は既定(tier=Standard・days=7・overwrite=false)で空欄', () => {
    const transfer = useTransferStore()
    expect(transfer.uuid).toBe('')
    expect(transfer.versionId).toBe('')
    expect(transfer.pin).toBe('')
    expect(transfer.out).toBe('')
    expect(transfer.tier).toBe('Standard')
    expect(transfer.days).toBe(7)
    expect(transfer.overwrite).toBe(false)
    expect(transfer.prefillApplied).toBe(false)
  })

  it('prefillFromQueryは?get=&version_id=を適用する(getが無い場合は適用しない)', () => {
    const transfer = useTransferStore()
    transfer.prefillFromQuery({
      get: '11111111-1111-1111-1111-111111111111',
      version_id: 'v1',
    })
    expect(transfer.uuid).toBe('11111111-1111-1111-1111-111111111111')
    expect(transfer.versionId).toBe('v1')
    expect(transfer.prefillApplied).toBe(true)

    // 同一対象の再適用はしない(手動入力をprefillで上書きしない)
    transfer.uuid = '22222222-2222-2222-2222-222222222222'
    transfer.prefillFromQuery({
      get: '11111111-1111-1111-1111-111111111111',
      version_id: 'v1',
    })
    expect(transfer.uuid).toBe('22222222-2222-2222-2222-222222222222')

    // 別対象の明示選択は新しい対象で再適用する(旧対象のversionIdは残さない)
    transfer.pin = 'oldpin'
    transfer.prefillFromQuery({ get: '99999999-9999-9999-9999-999999999999' })
    expect(transfer.uuid).toBe('99999999-9999-9999-9999-999999999999')
    expect(transfer.versionId).toBe('')
    expect(transfer.pin).toBe('')

    // getが無いqueryでは何も適用しない(別store instance=piniaを作り直して観測)
    setActivePinia(createPinia())
    const fresh = useTransferStore()
    fresh.prefillFromQuery({ version_id: 'v2' })
    expect(fresh.uuid).toBe('')
    expect(fresh.versionId).toBe('')
  })

  it('resetは既定値へ戻す', () => {
    const transfer = useTransferStore()
    transfer.prefillFromQuery({ get: 'u', version_id: 'v1' })
    transfer.out = '/tmp/x'
    transfer.tier = 'Bulk'
    transfer.days = 30
    transfer.overwrite = true
    transfer.reset()
    expect(transfer.uuid).toBe('')
    expect(transfer.versionId).toBe('')
    expect(transfer.out).toBe('')
    expect(transfer.tier).toBe('Standard')
    expect(transfer.days).toBe(7)
    expect(transfer.overwrite).toBe(false)
    expect(transfer.prefillApplied).toBe(false)
    expect(transfer.prefillKey).toBeNull()
  })
})

// [covers:frontend.transfer-get-form]
describe('transfer store(submitGet)', () => {
  it('uuid/outが空ならclientErrorを出しPOSTを行わない', async () => {
    const calls: string[] = []
    vi.stubGlobal(
      'fetch',
      vi.fn().mockImplementation((input: RequestInfo | URL, init?: RequestInit) => {
        const url = typeof input === 'string' ? input : input.toString()
        calls.push(`${init?.method ?? 'GET'} ${url}`)
        return Promise.resolve(responseOf(200, { jobs: [] }))
      }),
    )
    const transfer = useTransferStore()
    const ok = await transfer.submitGet()
    expect(ok).toBe(false)
    expect(transfer.clientError).toBe('対象uuidと出力dirを指定してください')
    expect(calls).toHaveLength(0)
  })

  it('submitはjobs storeのissueGetへ接続する(POST後に一覧再取得・issuing遷移)', async () => {
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
          return Promise.resolve(responseOf(200, { job: queuedJob }))
        }
        return Promise.resolve(responseOf(200, { jobs: [queuedJob] }))
      }),
    )
    const transfer = useTransferStore()
    transfer.prefillFromQuery({
      get: '11111111-1111-1111-1111-111111111111',
      version_id: 'v1',
    })
    transfer.out = '/tmp/restore'
    transfer.tier = 'Bulk'
    transfer.days = 30
    transfer.overwrite = true
    transfer.pin = 'keep'
    const pending = transfer.submitGet()
    expect(transfer.submitting).toBe(true)
    const ok = await pending
    expect(ok).toBe(true)
    expect(transfer.submitting).toBe(false)
    expect(transfer.clientError).toBeNull()
    expect(transfer.lastSubmitError).toBeNull()
    expect(calls).toEqual(['POST /api/jobs/get', 'GET /api/jobs'])
    // 発行bodyはform値(空のversion_idは送らない・pin/tier/days/overwriteを含む)
    expect(bodies[0]).toEqual({
      uuid: '11111111-1111-1111-1111-111111111111',
      version_id: 'v1',
      pin: 'keep',
      out: '/tmp/restore',
      tier: 'Bulk',
      days: 30,
      overwrite: true,
    })
    // jobs storeの一覧が再取得されている
    const jobs = useJobsStore()
    expect(jobs.items).toHaveLength(1)
  })

  it('発行失敗(422)はlastSubmitErrorへ反映する(jobs storeのlastIssueErrorの転写)', async () => {
    vi.stubGlobal(
      'fetch',
      vi.fn().mockImplementation(() =>
        Promise.resolve(
          responseOf(422, {
            error: { code: 'validation_failed', message: 'tierはStandard/Bulkのいずれかです' },
          }),
        ),
      ),
    )
    const transfer = useTransferStore()
    transfer.uuid = 'u'
    transfer.out = '/tmp/x'
    const ok = await transfer.submitGet()
    expect(ok).toBe(false)
    expect(transfer.submitting).toBe(false)
    expect(transfer.lastSubmitError?.code).toBe('validation_failed')
    const jobs = useJobsStore()
    expect(jobs.lastIssueError?.code).toBe('validation_failed')
  })
})

const dryRunResp = {
  connection: {
    profile: 'dev',
    bucket: 'api-bucket',
    prefix: 'amazing-backup/',
    recipients: ['SHA256:fp1'],
  },
  targets: [
    {
      path: '/nas/a.zip',
      name: 'a.zip',
      size: 2048,
      action: 'new',
      uuid: null,
      title: 'a',
      warning: null,
    },
  ],
  summary: { count: 1, total_size: 2048, skip: 0, re_put: 0, merge: 0, new: 1, failed: 0 },
}

const queuedPushJob = {
  id: 2,
  kind: 'push',
  status: 'queued',
  params: { targets: [{ path: '/nas/a.zip', title: 'a' }] },
  progress: { phase: 'queued', total: 1, done: 0, current_path: null },
  result: null,
  error: null,
  created_at: '2026-11-25T10:00:00+09:00',
  updated_at: '2026-11-25T10:00:00+09:00',
}

/** URL別にdry-run・発行・一覧を応じるfetch mock */
const stubTransferApis = (): { calls: string[]; bodies: unknown[] } => {
  const calls: string[] = []
  const bodies: unknown[] = []
  vi.stubGlobal(
    'fetch',
    vi.fn().mockImplementation((input: RequestInfo | URL, init?: RequestInit) => {
      const url = typeof input === 'string' ? input : input.toString()
      calls.push(`${init?.method ?? 'GET'} ${url}`)
      bodies.push(init?.body ? JSON.parse(String(init.body)) : null)
      if (url === '/api/push/dry-run') {
        return Promise.resolve(responseOf(200, dryRunResp))
      }
      if (url === '/api/jobs/push') {
        return Promise.resolve(responseOf(200, { job: queuedPushJob }))
      }
      return Promise.resolve(responseOf(200, { jobs: [queuedPushJob] }))
    }),
  )
  return { calls, bodies }
}

// [covers:frontend.transfer-upload-form]
describe('transfer store(upload form・prefill・dry-run確認ガード・submitPush)', () => {
  it('?upload=<path>でpathと拡張子を除いたbasename由来のtitle候補をprefillする(同一対象は再適用せず別対象は再適用)', () => {
    const transfer = useTransferStore()
    expect(transfer.uploadTargets).toEqual([{ path: '', title: '' }])

    transfer.prefillFromQuery({ upload: '/nas/photos/2026-trip.zip' })
    expect(transfer.uploadTargets).toEqual([
      { path: '/nas/photos/2026-trip.zip', title: '2026-trip' },
    ])

    // 同一uploadの再適用はしない(手動編集を保護)
    transfer.updateUploadTarget(0, { title: 'custom-title' })
    transfer.prefillFromQuery({ upload: '/nas/photos/2026-trip.zip' })
    expect(transfer.uploadTargets[0]?.title).toBe('custom-title')

    // 別pathの明示選択では再適用する
    transfer.prefillFromQuery({ upload: '/nas/docs/report.7z' })
    expect(transfer.uploadTargets).toEqual([{ path: '/nas/docs/report.7z', title: 'report' }])
  })

  it('行の追加・削除・空欄/重複のclient側検証と、dry-run未確認時のsubmitPush拒否', async () => {
    const { calls } = stubTransferApis()
    const transfer = useTransferStore()
    transfer.addUploadTarget()
    expect(transfer.uploadTargets).toHaveLength(2)
    transfer.removeUploadTarget(1)
    expect(transfer.uploadTargets).toHaveLength(1)

    // 空欄と重複行はserverへの送信前に案内する(server検証の前置き)
    expect(await transfer.runDryRun()).toBe(false)
    expect(transfer.uploadClientError).toBeTruthy()
    transfer.updateUploadTarget(0, { path: '/nas/a.zip', title: 'a' })
    transfer.addUploadTarget()
    transfer.updateUploadTarget(1, { path: '/nas/a.zip' })
    expect(await transfer.runDryRun()).toBe(false)
    expect(transfer.uploadClientError).toMatch(/重複/)
    transfer.removeUploadTarget(1)
    expect(calls).toHaveLength(0)

    // 未確認のsubmitPushはAPIを呼ばずuploadClientErrorを出す
    expect(transfer.isUploadConfirmed).toBe(false)
    expect(await transfer.submitPush()).toBe(false)
    expect(transfer.uploadClientError).toBeTruthy()
    expect(calls).toHaveLength(0)
  })

  it('runDryRunで接続先・予測を保持して確認済みへ遷移し、対象変更や取消で失効し、確認済みsubmitPushで発行する', async () => {
    const { calls, bodies } = stubTransferApis()
    const transfer = useTransferStore()
    // 空欄時はPOSTしない
    expect(await transfer.runDryRun()).toBe(false)
    expect(transfer.uploadClientError).toBeTruthy()
    expect(calls).toHaveLength(0)

    transfer.updateUploadTarget(0, { path: '/nas/a.zip', title: 'a' })
    expect(await transfer.runDryRun()).toBe(true)
    expect(transfer.dryRunResult?.connection.recipients).toEqual(['SHA256:fp1'])
    expect(transfer.isUploadConfirmed).toBe(true)

    // 対象path・titleを書き換えるとdry-run結果と確認済み状態が失効する
    //(確認内容と異なるtargetsを未確認のままpushできない)
    transfer.updateUploadTarget(0, { path: '/nas/other.zip' })
    expect(transfer.dryRunResult).toBeNull()
    expect(transfer.isUploadConfirmed).toBe(false)
    transfer.updateUploadTarget(0, { path: '/nas/a.zip', title: 'a' })
    expect(await transfer.runDryRun()).toBe(true)
    transfer.updateUploadTarget(0, { title: 'renamed' })
    expect(transfer.dryRunResult).toBeNull()
    expect(transfer.isUploadConfirmed).toBe(false)

    // 行の追加・削除・明示cancelでも失効する
    transfer.updateUploadTarget(0, { title: 'a' })
    expect(await transfer.runDryRun()).toBe(true)
    transfer.addUploadTarget()
    expect(transfer.dryRunResult).toBeNull()
    expect(transfer.isUploadConfirmed).toBe(false)
    transfer.removeUploadTarget(1)
    expect(await transfer.runDryRun()).toBe(true)
    transfer.cancelUploadConfirm()
    expect(transfer.dryRunResult).toBeNull()
    expect(transfer.isUploadConfirmed).toBe(false)

    // 確認済みsubmitPushでPOST /api/jobs/push + GET /api/jobs
    expect(await transfer.runDryRun()).toBe(true)
    calls.length = 0
    bodies.length = 0
    const ok = await transfer.submitPush()
    expect(ok).toBe(true)
    expect(calls).toEqual(['POST /api/jobs/push', 'GET /api/jobs'])
    expect(bodies[0]).toEqual({ targets: [{ path: '/nas/a.zip', title: 'a' }] })
    expect(transfer.isUploadConfirmed).toBe(false)
    expect(transfer.uploadClientError).toBeNull()
  })

  it('古い入力世代のdry-run応答は破棄する。確認済みtargetsと現在の入力が不一致なら発行しない', async () => {
    // 連打・編集直後の後着対策: 要求後に編集があった場合、その前の要求の応答は破棄する
    let resolveStale: (response: Response) => void = () => {}
    const staleFetch = vi
      .fn()
      .mockImplementationOnce(
        () =>
          new Promise<Response>((resolve) => {
            resolveStale = resolve
          }),
      )
      .mockImplementationOnce(() => Promise.resolve(responseOf(200, dryRunResp)))
    vi.stubGlobal('fetch', staleFetch)
    const transfer = useTransferStore()
    transfer.updateUploadTarget(0, { path: '/nas/a.zip', title: 'first' })
    const stale = transfer.runDryRun()
    expect(transfer.dryRunning).toBe(true)
    transfer.updateUploadTarget(0, { title: 'second' })
    expect(await transfer.runDryRun()).toBe(true)
    expect(transfer.isUploadConfirmed).toBe(true)
    resolveStale(responseOf(200, dryRunResp))
    expect(await stale).toBe(false)
    expect(staleFetch).toHaveBeenCalledTimes(2)
    // 古い応答でdry-run結果・確認済み状態は書き換わらない
    expect(transfer.isUploadConfirmed).toBe(true)
    expect(transfer.dryRunning).toBe(false)

    // 確認時点のsnapshotと送信targetsが完全一致しない場合は発行しない
    //(actionを経由しない直接変更で不一致を作って模擬)
    const jobs = useJobsStore()
    const issuePush = vi.spyOn(jobs, 'issuePush')
    transfer.uploadTargets[0]!.title = 'tampered'
    expect(transfer.isUploadConfirmed).toBe(false)
    expect(await transfer.submitPush()).toBe(false)
    expect(transfer.uploadClientError).toBeTruthy()
    expect(issuePush).not.toHaveBeenCalled()
  })

  it('確認済みsubmitPushの発行失敗(422)はlastUploadSubmitErrorへ反映する', async () => {
    vi.stubGlobal(
      'fetch',
      vi.fn().mockImplementation((input: RequestInfo | URL, init?: RequestInit) => {
        const url = typeof input === 'string' ? input : input.toString()
        if (init?.method === 'POST' && url === '/api/push/dry-run') {
          return Promise.resolve(responseOf(200, dryRunResp))
        }
        return Promise.resolve(
          responseOf(422, {
            error: { code: 'validation_failed', message: 'pathが重複しています' },
          }),
        )
      }),
    )
    const transfer = useTransferStore()
    transfer.updateUploadTarget(0, { path: '/nas/a.zip', title: 'a' })
    expect(await transfer.runDryRun()).toBe(true)
    expect(await transfer.submitPush()).toBe(false)
    expect(transfer.uploadSubmitting).toBe(false)
    expect(transfer.lastUploadSubmitError?.code).toBe('validation_failed')
  })
})
