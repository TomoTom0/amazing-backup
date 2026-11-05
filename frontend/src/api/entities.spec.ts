import { afterEach, describe, expect, it, vi } from 'vitest'
import {
  fetchEntities,
  fetchEntityDetail,
  fetchEntityVersions,
  isEntitiesResponse,
} from './client'

// GET /api/entities・:uuid・:uuid/versions応答の型ガードとentities client
// (TASK-9設計6章・7章・コードレビュー指摘高4=EntitiesView/EntityDetailViewの
// 本格画面)。実装前のためclient.tsへentities系は未実装=本fileのtestは実行failを
// 許容(実装後に緑化)。実描画は実browser観測のため条件書frontend.tomlのexcluded
// 参照(UI誘導状態はstores/entities.tsのgetterで担保)。

const summary = {
  uuid: '0192a4f0-0000-7000-8000-000000000001',
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
  pins: [{ pin: 'default', date: '2026-11-05', source_size: 1024 }],
}

const detailBody = {
  ...summary,
  version_list: [
    {
      date: '2026-11-05',
      version_ts: '2026-11-05T12:00:00Z',
      version_id: 'vid-2',
      batch_id: 'batch-2',
      source_size: 1024,
    },
    {
      date: '2026-11-04',
      version_ts: '2026-11-04T12:00:00Z',
      version_id: 'vid-1',
      batch_id: 'batch-1',
      source_size: 512,
    },
  ],
}

const versionsBody = {
  uuid: summary.uuid,
  title: summary.title,
  locations: summary.locations,
  versions: [
    {
      date: '2026-11-05',
      version_ts: '2026-11-05T12:00:00Z',
      version_id: 'vid-2',
      source_size: 1024,
      state: '現行',
    },
    {
      date: '2026-11-04',
      version_ts: '2026-11-04T12:00:00Z',
      version_id: 'vid-1',
      source_size: 512,
      state: 'noncurrent',
    },
  ],
  pins: summary.pins,
}

const responseOf = (status: number, body: unknown): Response =>
  new Response(JSON.stringify(body), {
    status,
    headers: { 'Content-Type': 'application/json' },
  })

afterEach(() => {
  vi.unstubAllGlobals()
})

// [covers:frontend.entities-client]
describe('GET /api/entities応答の型ガード', () => {
  it('応答形式(entities配列+page+total_pages+要素schema)を判定する', () => {
    expect(isEntitiesResponse({ entities: [summary], page: 1, total_pages: 1 })).toBe(true)
    expect(isEntitiesResponse({ entities: [], page: 1, total_pages: 1 })).toBe(true)
    // page・total_pagesの型不一致はfalse
    expect(isEntitiesResponse({ entities: [], page: '1', total_pages: 1 })).toBe(false)
    expect(isEntitiesResponse({ entities: [], page: 1, total_pages: null })).toBe(false)
    // 要素schemaの不一致(title・versions件数・locations形式)はfalse
    expect(
      isEntitiesResponse({ entities: [{ ...summary, title: 1 }], page: 1, total_pages: 1 }),
    ).toBe(false)
    expect(
      isEntitiesResponse({ entities: [{ ...summary, versions: '2' }], page: 1, total_pages: 1 }),
    ).toBe(false)
    expect(
      isEntitiesResponse({
        entities: [{ ...summary, locations: '/data/a.zip' }],
        page: 1,
        total_pages: 1,
      }),
    ).toBe(false)
    // entities欠落・nullはfalse
    expect(isEntitiesResponse({ page: 1, total_pages: 1 })).toBe(false)
    expect(isEntitiesResponse(null)).toBe(false)
  })
})

// [covers:frontend.entities-client]
describe('entities clientの取得(fetchEntities/detail/versions)', () => {
  it('fetchEntitiesは値のある絞り込みparamのみURL queryへ直列化する', async () => {
    const captured: string[] = []
    vi.stubGlobal(
      'fetch',
      vi.fn().mockImplementation((input: RequestInfo | URL) => {
        captured.push(typeof input === 'string' ? input : input.toString())
        return Promise.resolve(responseOf(200, { entities: [], page: 1, total_pages: 1 }))
      }),
    )
    // param無しはquery無しのURLへ
    await fetchEntities()
    // query/category/label/pageはそれぞれURL queryへ(順序は問わない)
    await fetchEntities({ query: 'beta', category: 'docs', label: 'work', page: 2 })
    // 値の無いparamは付けない(pageのみの単独指定)
    await fetchEntities({ page: 3 })
    expect(captured[0]).toBe('/api/entities')
    expect(captured[1].startsWith('/api/entities?')).toBe(true)
    expect(captured[1]).toContain('query=beta')
    expect(captured[1]).toContain('category=docs')
    expect(captured[1]).toContain('label=work')
    expect(captured[1]).toContain('page=2')
    expect(captured[2]).toBe('/api/entities?page=3')
  })

  it('fetchEntityDetailは/api/entities/<uuid>を呼び不在404はerror扱いにする', async () => {
    const captured: string[] = []
    vi.stubGlobal(
      'fetch',
      vi.fn().mockImplementation((input: RequestInfo | URL) => {
        captured.push(typeof input === 'string' ? input : input.toString())
        if (captured.length === 1) {
          return Promise.resolve(responseOf(200, detailBody))
        }
        return Promise.resolve(
          responseOf(404, { error: { code: 'not_found', message: '対象のentityが見つかりません' } }),
        )
      }),
    )
    const ok = await fetchEntityDetail(summary.uuid)
    expect(ok.ok).toBe(true)
    if (ok.ok) {
      expect(ok.data.uuid).toBe(summary.uuid)
      expect(ok.data.version_list.length).toBe(2)
    }
    const missing = await fetchEntityDetail('99999999-9999-9999-9999-999999999999')
    expect(missing.ok).toBe(false)
    if (!missing.ok) {
      expect(missing.error.code).toBe('not_found')
      expect(missing.httpStatus).toBe(404)
    }
    expect(captured[0]).toBe(`/api/entities/${summary.uuid}`)
    expect(captured[1]).toBe('/api/entities/99999999-9999-9999-9999-999999999999')
  })

  it('fetchEntityVersionsは/api/entities/<uuid>/versionsを呼びstate付きの版一覧を返す', async () => {
    const captured: string[] = []
    vi.stubGlobal(
      'fetch',
      vi.fn().mockImplementation((input: RequestInfo | URL) => {
        captured.push(typeof input === 'string' ? input : input.toString())
        return Promise.resolve(responseOf(200, versionsBody))
      }),
    )
    const result = await fetchEntityVersions(summary.uuid)
    expect(result.ok).toBe(true)
    if (result.ok) {
      expect(result.data.versions.length).toBe(2)
      expect(result.data.versions[0].state).toBe('現行')
      expect(result.data.versions[1].state).toBe('noncurrent')
    }
    expect(captured[0]).toBe(`/api/entities/${summary.uuid}/versions`)
  })
})
