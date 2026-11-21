import { afterEach, describe, expect, it, vi } from 'vitest'
import { updateEntityLocation } from './client'

// POST /api/entities/:uuid/locationのclient(TASK-17設計2.2(folder変更: 所在行ごとの
// category/label編集form→POST /api/entities/:uuid/location)・3.2(body
// {source_path,category,label}・対象location完全一致指定。不在は404)・9章W3=TASK-22)。

const UUID = '0192a4f0-0000-7000-8000-000000000001'

const detailAfterLocation = {
  uuid: UUID,
  title: 'a.zip',
  locations: [
    {
      source_path: '/data/a.zip',
      category: 'photos',
      label: 'sub',
      identity_hash: 'sha256:AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=',
      last_backup: '2026-11-05T12:00:00+09:00',
    },
  ],
  versions: 2,
  latest_date: '2026-11-05',
  latest_size: 1024,
  pins: [],
  tags: [],
  collections: [],
  version_list: [],
}

const responseOf = (status: number, body: unknown): Response =>
  new Response(JSON.stringify(body), {
    status,
    headers: { 'Content-Type': 'application/json' },
  })

afterEach(() => {
  vi.unstubAllGlobals()
})

// [covers:frontend.entity-location-edit-client-and-store]
describe('POST /api/entities/:uuid/location client(updateEntityLocation)', () => {
  it('POST /api/entities/<uuid>/locationへ{source_path,category,label}bodyを送り応答を返す', async () => {
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
        return Promise.resolve(responseOf(200, detailAfterLocation))
      }),
    )
    const result = await updateEntityLocation(UUID, '/data/a.zip', 'photos', 'sub')
    expect(calls).toEqual([`POST /api/entities/${UUID}/location`])
    expect(bodies).toEqual([
      { source_path: '/data/a.zip', category: 'photos', label: 'sub' },
    ])
    expect(result.ok).toBe(true)
    if (result.ok) {
      expect(result.data.locations[0]?.category).toBe('photos')
      expect(result.data.locations[0]?.label).toBe('sub')
    }
  })

  it('label空文字(ラベルなし)もそのまま送る・404/422はerror扱い', async () => {
    const bodies: unknown[] = []
    vi.stubGlobal(
      'fetch',
      vi.fn().mockImplementation((input: RequestInfo | URL, init?: RequestInit) => {
        const parsed: unknown =
          init?.body === undefined || init?.body === null ? null : JSON.parse(String(init.body))
        bodies.push(parsed)
        return Promise.resolve(responseOf(200, detailAfterLocation))
      }),
    )
    const result = await updateEntityLocation(UUID, '/data/a.zip', 'photos', '')
    expect(result.ok).toBe(true)
    expect(bodies).toEqual([{ source_path: '/data/a.zip', category: 'photos', label: '' }])

    vi.stubGlobal(
      'fetch',
      vi.fn().mockResolvedValue(
        responseOf(404, {
          error: { code: 'not_found', message: '対象が見つかりません' },
        }),
      ),
    )
    const missing = await updateEntityLocation(UUID, '/data/elsewhere', 'photos', '')
    expect(missing.ok).toBe(false)
    if (!missing.ok) {
      expect(missing.error.code).toBe('not_found')
      expect(missing.httpStatus).toBe(404)
    }

    vi.stubGlobal(
      'fetch',
      vi.fn().mockResolvedValue(
        responseOf(422, {
          error: { code: 'validation_failed', message: '入力値の規則に違反しています' },
        }),
      ),
    )
    const invalid = await updateEntityLocation(UUID, '/data/a.zip', 'Bad', '')
    expect(invalid.ok).toBe(false)
    if (!invalid.ok) {
      expect(invalid.error.code).toBe('validation_failed')
    }
  })
})
