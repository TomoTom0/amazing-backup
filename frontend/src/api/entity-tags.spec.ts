import { afterEach, describe, expect, it, vi } from 'vitest'
import { putEntityTags } from './client'

// PUT /api/entities/:uuid/tagsのclient(TASK-17設計2.2(詳細のtag編集=tag追加/削除
// (chipのx)→PUT /api/entities/:uuid/tags)・3.2(body {tags:[...]}・冪等・tag名規則
// 違反は422。応答は操作後の対象状態=読取系と同一schema)・9章W2=TASK-19)。
// 実装前のためclient.tsへputEntityTagsは未実装=本fileのtestは実行failを許容
// (実装後に緑化)。応答のtags/collections拡張はfrontend.entities-clientの改訂
// (EntitySummaryへtags・EntityDetailへcollections)の解釈に整合。

const UUID = '0192a4f0-0000-7000-8000-000000000001'

const detailAfterPut = {
  uuid: UUID,
  title: 'a.zip',
  locations: [
    {
      source_path: '/data/a.zip',
      category: 'docs',
      label: 'work',
      identity_hash: 'sha256:AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=',
      last_backup: '2026-11-05T12:00:00+09:00',
    },
  ],
  versions: 2,
  latest_date: '2026-11-05',
  latest_size: 1024,
  pins: [],
  tags: ['work', 'nas'],
  collections: ['weekly'],
  version_list: [
    {
      date: '2026-11-05',
      version_ts: '2026-11-05T12:00:00Z',
      version_id: 'vid-2',
      batch_id: 'batch-2',
      source_size: 1024,
    },
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

// [covers:frontend.entity-tags-edit-client-and-store]
describe('PUT /api/entities/:uuid/tags client(putEntityTags)', () => {
  it('PUT /api/entities/<uuid>/tagsへ{tags}bodyを送り応答(操作後の対象状態)を返す', async () => {
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
        return Promise.resolve(responseOf(200, detailAfterPut))
      }),
    )
    const result = await putEntityTags(UUID, ['work', 'nas'])
    expect(calls).toEqual([`PUT /api/entities/${UUID}/tags`])
    expect(bodies).toEqual([{ tags: ['work', 'nas'] }])
    expect(result.ok).toBe(true)
    if (result.ok) {
      // 応答は操作後の対象状態(tags/collections付きの詳細形式)
      expect(result.data.uuid).toBe(UUID)
      expect(result.data.tags).toEqual(['work', 'nas'])
      expect(result.data.collections).toEqual(['weekly'])
    }
  })

  it('tag名規則違反の422 validation_failed・不在uuidの404 not_foundはerror扱い', async () => {
    vi.stubGlobal(
      'fetch',
      vi
        .fn()
        .mockImplementationOnce(() =>
          Promise.resolve(
            responseOf(422, {
              error: { code: 'validation_failed', message: 'tag名の規則に違反しています' },
            }),
          ),
        )
        .mockImplementationOnce(() =>
          Promise.resolve(
            responseOf(404, { error: { code: 'not_found', message: '対象のentityが見つかりません' } }),
          ),
        ),
    )
    const invalid = await putEntityTags(UUID, ['BAD TAG'])
    expect(invalid.ok).toBe(false)
    if (!invalid.ok) {
      expect(invalid.error.code).toBe('validation_failed')
      expect(invalid.httpStatus).toBe(422)
    }
    const missing = await putEntityTags('99999999-9999-9999-9999-999999999999', ['work'])
    expect(missing.ok).toBe(false)
    if (!missing.ok) {
      expect(missing.error.code).toBe('not_found')
      expect(missing.httpStatus).toBe(404)
    }
  })
})
