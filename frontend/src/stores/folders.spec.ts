import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { createPinia, setActivePinia } from 'pinia'
import { useFoldersStore } from './folders'

// folders store(TASK-17設計2.2・9章W2=TASK-19): folder構造ビュー=左tree・右に
// 選択folder配下のentity一覧。tree getterはGET /api/folders応答
// ({folders:[{label,path,entity_count}]}・階層集約済み)のpath区切りから入れ子を
// 組む(folder tree部品のdata基盤。部品の実描画はexcluded)。実装前のため
// src/stores/folders.tsは未実装=本fileのtestは実行failを許容(実装後に緑化)。

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

beforeEach(() => {
  setActivePinia(createPinia())
})

afterEach(() => {
  vi.unstubAllGlobals()
})

// [covers:frontend.folders-client-and-store]
describe('folders store(fetchFolders・tree getter)', () => {
  it('成功時はfolder一覧を保持しlastErrorはnull', async () => {
    vi.stubGlobal(
      'fetch',
      vi.fn().mockImplementation(() => Promise.resolve(responseOf(200, foldersBody))),
    )
    const folders = useFoldersStore()
    await folders.fetchFolders()
    expect(folders.items).toHaveLength(3)
    expect(folders.items[1]?.path).toBe('docs/2026')
    expect(folders.lastError).toBeNull()
  })

  it('tree getterはpathの階層から入れ子node配列を組む(兄弟はpath順・nodeはentity_countを保持)', async () => {
    vi.stubGlobal(
      'fetch',
      vi.fn().mockImplementation(() => Promise.resolve(responseOf(200, foldersBody))),
    )
    const folders = useFoldersStore()
    await folders.fetchFolders()
    const tree = folders.tree
    expect(tree).toHaveLength(2)
    // 1つ目のroot: docs(直属entity_count=2)の子にdocs/2026が入る
    expect(tree[0]?.label).toBe('docs')
    expect(tree[0]?.path).toBe('docs')
    expect(tree[0]?.entity_count).toBe(2)
    expect(tree[0]?.children).toHaveLength(1)
    expect(tree[0]?.children[0]?.label).toBe('2026')
    expect(tree[0]?.children[0]?.path).toBe('docs/2026')
    expect(tree[0]?.children[0]?.entity_count).toBe(1)
    // 2つ目のroot: photos(子なし)
    expect(tree[1]?.label).toBe('photos')
    expect(tree[1]?.children).toEqual([])
  })

  it('失敗時はlastErrorを保持しitemsは空へ戻る(空dataを表示しない)', async () => {
    // 非空一覧を保持した状態での再取得失敗=保持済みitemsの消去も観測する
    vi.stubGlobal(
      'fetch',
      vi
        .fn()
        .mockImplementationOnce(() => Promise.resolve(responseOf(200, foldersBody)))
        .mockImplementationOnce(() =>
          Promise.resolve(
            responseOf(401, { error: { code: 'unauthenticated', message: '認証が必要です' } }),
          ),
        ),
    )
    const folders = useFoldersStore()
    await folders.fetchFolders()
    expect(folders.items).toHaveLength(3)
    await folders.fetchFolders()
    expect(folders.items).toEqual([])
    expect(folders.lastError?.code).toBe('unauthenticated')
  })
})

// [covers:frontend.view-options-fetch-state]
describe('folders storeの取得状態(fetchState・W2レビュー指摘中8)', () => {
  it('失敗=error後の再fetchでokへ復帰する(再試行導線)', async () => {
    vi.stubGlobal(
      'fetch',
      vi
        .fn()
        .mockImplementationOnce(() =>
          Promise.resolve(
            responseOf(500, { error: { code: 'internal', message: '内部エラーです' } }),
          ),
        )
        .mockImplementationOnce(() => Promise.resolve(responseOf(200, foldersBody))),
    )
    const folders = useFoldersStore()
    await folders.fetchFolders()
    expect(folders.fetchState).toBe('error')
    await folders.fetchFolders()
    expect(folders.fetchState).toBe('ok')
    expect(folders.items).toHaveLength(3)
    expect(folders.lastError).toBeNull()
  })
})

// [covers:frontend.folders-labelless-node]
describe('folders storeのtree getter・ラベルなし行(W2レビュー指摘中10)', () => {
  it('応答先頭行(label=""・path="")は「(ラベルなし)」root nodeになり通常folderの親とはならない', async () => {
    const withLabelless = {
      folders: [
        { label: '', path: '', entity_count: 2 },
        ...foldersBody.folders,
      ],
    }
    vi.stubGlobal(
      'fetch',
      vi.fn().mockImplementation(() => Promise.resolve(responseOf(200, withLabelless))),
    )
    const folders = useFoldersStore()
    await folders.fetchFolders()
    const tree = folders.tree
    // 先頭はラベルなしnode(path=""・子無し)
    expect(tree).toHaveLength(3)
    expect(tree[0]?.label).toBe('(ラベルなし)')
    expect(tree[0]?.path).toBe('')
    expect(tree[0]?.entity_count).toBe(2)
    expect(tree[0]?.children).toEqual([])
    // 通常folder(docs・photos)はラベルなしnodeの子にならずrootに並ぶ
    expect(tree[1]?.label).toBe('docs')
    expect(tree[1]?.children).toHaveLength(1)
    expect(tree[2]?.label).toBe('photos')
  })
})
