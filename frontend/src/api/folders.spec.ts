import { afterEach, describe, expect, it, vi } from 'vitest'
import { fetchFolders, isFoldersResponse } from './client'

// GET /api/folders応答の型ガードとfolders client(TASK-17設計3.2(GET /api/folders=
// {folders:[{label,path,entity_count}]}・labelのdirectory区切りで階層導出)・2.2
// (folder構造ビュー=左にtree・右に選択folder配下のentity一覧)・9章W2=TASK-19)。
// 実装前のためclient.tsへfolders系は未実装=本fileのtestは実行failを許容
// (実装後に緑化)。

const foldersBody = {
  folders: [
    { label: 'docs', path: 'docs', entity_count: 2 },
    { label: 'docs/2026', path: 'docs/2026', entity_count: 1 },
    { label: 'photos', path: 'photos', entity_count: 5 },
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

// [covers:frontend.folders-client-and-store]
describe('GET /api/folders応答の型ガード(isFoldersResponse)', () => {
  it('形式一致のみtrue(folders配列+各要素のlabel/path/entity_count)', () => {
    expect(isFoldersResponse(foldersBody)).toBe(true)
    expect(isFoldersResponse({ folders: [] })).toBe(true)
    // folders欠落・folders非配列・要素のfield欠落・型不一致・nullはfalse
    expect(isFoldersResponse({})).toBe(false)
    expect(isFoldersResponse({ folders: 'x' })).toBe(false)
    expect(isFoldersResponse({ folders: [{ label: 'docs', entity_count: 1 }] })).toBe(false)
    expect(
      isFoldersResponse({ folders: [{ label: 1, path: 'docs', entity_count: 1 }] }),
    ).toBe(false)
    expect(
      isFoldersResponse({ folders: [{ label: 'docs', path: 'docs', entity_count: '2' }] }),
    ).toBe(false)
    expect(isFoldersResponse(null)).toBe(false)
  })
})

// [covers:frontend.folders-client-and-store]
describe('folders client(fetchFolders)', () => {
  it('GET /api/foldersへ向かい200応答のfolder一覧を返す', async () => {
    const calls: string[] = []
    vi.stubGlobal(
      'fetch',
      vi.fn().mockImplementation((input: RequestInfo | URL, init?: RequestInit) => {
        const url = typeof input === 'string' ? input : input.toString()
        calls.push(`${init?.method ?? 'GET'} ${url}`)
        return Promise.resolve(responseOf(200, foldersBody))
      }),
    )
    const result = await fetchFolders()
    expect(calls).toEqual(['GET /api/folders'])
    expect(result.ok).toBe(true)
    if (result.ok) {
      expect(result.data.folders).toHaveLength(3)
      expect(result.data.folders[1]?.label).toBe('docs/2026')
      expect(result.data.folders[1]?.path).toBe('docs/2026')
      expect(result.data.folders[1]?.entity_count).toBe(1)
    }
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
    const result = await fetchFolders()
    expect(result.ok).toBe(false)
    if (!result.ok) {
      expect(result.error.code).toBe('unauthenticated')
      expect(result.httpStatus).toBe(401)
    }
  })
})
