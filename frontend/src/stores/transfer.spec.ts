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
    transfer.prefillFromQuery({ get: '99999999-9999-9999-9999-999999999999' })
    expect(transfer.uuid).toBe('99999999-9999-9999-9999-999999999999')
    expect(transfer.versionId).toBe('')

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
