import { defineStore } from 'pinia'
import { fetchFolders, type ApiError, type FolderItem } from '../api/client'
import type { OptionsFetchState } from './tags'

/** folder treeのnode(path='/'区切りの階層組立結果。label=その階層の表示名
 *  (path最終区切り要素)・entity_count=当該node配下のentity数(API応答の値=子folder
 *  配下を含む合計)・children=入れ子の子node。実描画はfolder tree部品(excluded)) */
export type FolderNode = {
  label: string
  path: string
  entity_count: number
  children: FolderNode[]
}

/**
 * folders store(TASK-17設計2.2・9章W2): folder構造ビュー=左tree・右に選択folderの
 * entity一覧。tree getterはGET /api/folders応答(階層集約済み・path=label絞り込み値
 * と一致)のpath区切りから入れ子を組む。取得失敗時はitemsを空へ戻しlastErrorを
 * 保持する(空dataを表示しない)。
 */
export const useFoldersStore = defineStore('folders', {
  state: () => ({
    /** folder一覧(未取得・取得失敗時は空) */
    items: [] as FolderItem[],
    /** 取得状態(失敗後もfetchFoldersの再呼出で再取得できる=Viewの再試行button基盤) */
    fetchState: 'not_fetched' as OptionsFetchState,
    lastError: null as ApiError | null,
  }),
  getters: {
    /** itemsのpathを'/'区切りで入れ子node配列へ組む(兄弟はpath辞書順。
     *  中間nodeもAPI応答に1行ずつ含まれるため親は既出nodeから解決できる)。
     *  W2レビュー指摘中10対応: 応答先頭行のラベルなし行(path="")は
     *  「(ラベルなし)」nodeとしてtreeの先頭に組み・他folderの親とはしない
     *  (parentPathの""はroot扱い。選択時はlabel=空文字絞り込みへ接続) */
    tree(state): FolderNode[] {
      const roots: FolderNode[] = []
      const nodes = new Map<string, FolderNode>()
      const sorted = [...state.items].sort((a, b) => (a.path < b.path ? -1 : 1))
      for (const item of sorted) {
        const segments = item.path.split('/')
        const node: FolderNode = {
          label: item.path === '' ? '(ラベルなし)' : segments[segments.length - 1] ?? item.path,
          path: item.path,
          entity_count: item.entity_count,
          children: [],
        }
        nodes.set(item.path, node)
        const parentPath =
          item.path.includes('/') ? item.path.slice(0, item.path.lastIndexOf('/')) : null
        const parent = parentPath === null ? undefined : nodes.get(parentPath)
        if (parent === undefined) {
          roots.push(node)
        } else {
          parent.children.push(node)
        }
      }
      return roots
    },
  },
  actions: {
    /** folder一覧の取得(失敗時はitemsを空へ戻しlastErrorを保持) */
    async fetchFolders(): Promise<void> {
      const result = await fetchFolders()
      if (result.ok) {
        this.items = result.data.folders
        this.fetchState = 'ok'
        this.lastError = null
        return
      }
      this.items = []
      this.fetchState = 'error'
      this.lastError = result.error
    },
  },
})
