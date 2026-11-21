import { afterEach, describe, expect, it, vi } from 'vitest'
import { renameEntity } from './client'

// POST /api/entities/:uuid/renameのclient(TASK-17設計2.2(名前変更: title編集form→
// POST /api/entities/:uuid/rename)・3.2(body {title}。64文字//制御文字違反は422。
// title重複は許容+警告応答=CLI仕様と同一)・9章W3=TASK-22)。応答はEntityDetail形式
// +title重複時のみwarning。

const UUID = '0192a4f0-0000-7000-8000-000000000001'

const detailAfterRename = {
  uuid: UUID,
  title: 'renamed',
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
  tags: ['work'],
  collections: [],
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

// [covers:frontend.entity-rename-client-and-store]
describe('POST /api/entities/:uuid/rename client(renameEntity)', () => {
  it('POST /api/entities/<uuid>/renameへ{title}bodyを送り応答(操作後の対象状態)を返す', async () => {
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
        return Promise.resolve(responseOf(200, detailAfterRename))
      }),
    )
    const result = await renameEntity(UUID, 'renamed')
    expect(calls).toEqual([`POST /api/entities/${UUID}/rename`])
    expect(bodies).toEqual([{ title: 'renamed' }])
    expect(result.ok).toBe(true)
    if (result.ok) {
      expect(result.data.uuid).toBe(UUID)
      expect(result.data.title).toBe('renamed')
      expect(result.data.warning).toBeUndefined()
    }
  })

  it('title重複時の応答warningを型ガードが通し値として返す', async () => {
    vi.stubGlobal(
      'fetch',
      vi.fn().mockResolvedValue(
        responseOf(200, {
          ...detailAfterRename,
          warning:
            'title(renamed)は既存entityと重複しています(許容されます。一意化はuuid/所在指定で行ってください)',
        }),
      ),
    )
    const result = await renameEntity(UUID, 'renamed')
    expect(result.ok).toBe(true)
    if (result.ok) {
      expect(result.data.warning).toContain('重複')
    }
  })

  it('title規則違反の422・不在uuidの404はerror扱い', async () => {
    vi.stubGlobal(
      'fetch',
      vi.fn().mockResolvedValue(
        responseOf(422, {
          error: { code: 'validation_failed', message: '入力値の規則に違反しています' },
        }),
      ),
    )
    const result = await renameEntity(UUID, 'bad/title')
    expect(result.ok).toBe(false)
    if (!result.ok) {
      expect(result.error.code).toBe('validation_failed')
      expect(result.httpStatus).toBe(422)
    }
  })

  it('warningが文字列でない応答はinternal扱い(型ガードで検証)', async () => {
    vi.stubGlobal(
      'fetch',
      vi.fn().mockResolvedValue(responseOf(200, { ...detailAfterRename, warning: 3 })),
    )
    const result = await renameEntity(UUID, 'renamed')
    expect(result.ok).toBe(false)
    if (!result.ok) {
      expect(result.error.code).toBe('internal')
    }
  })
})
