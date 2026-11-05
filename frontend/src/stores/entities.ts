import { defineStore } from 'pinia'
import {
  fetchEntities,
  fetchEntityDetail,
  fetchEntityVersions,
  type ApiError,
  type EntityDetail,
  type EntitySummary,
  type EntityVersionsResponse,
} from '../api/client'

/**
 * entities store(TASK-9設計7章=EntitiesView(一覧・検索・絞込・paging)・
 * EntityDetailView・コードレビュー指摘高4)。一覧・絞り込み状態とUI誘導状態
 * (entitiesView getter)・詳細/versions状態を保持する。取得失敗時はitemsを空へ
 * 戻しlastErrorを保持する(空dataを表示しない=fail-closedと共通)。
 */
export const useEntitiesStore = defineStore('entities', {
  state: () => ({
    /** entity一覧(未取得・取得失敗時は空) */
    items: [] as EntitySummary[],
    /** 検索語(空文字列=絞り込み無し) */
    query: '',
    /** category絞り込み(null=絞り込み無し) */
    category: null as string | null,
    /** 現在page(1始点) */
    page: 1,
    totalPages: 1,
    loading: false,
    /** 一覧取得を1回以上成功させたか(0件の成功もresult表示のためのflag) */
    fetched: false,
    lastError: null as ApiError | null,
    /** 選択中entity(EntityDetailView表示対象) */
    selectedUuid: null as string | null,
    /** 詳細(version_list付き。不在404時は保持しない) */
    detail: null as EntityDetail | null,
    /** 詳細・versions取得のerror(detail・versionsの共通error状態) */
    detailError: null as ApiError | null,
    /** 版一覧(state=現行/noncurrent付き) */
    versions: null as EntityVersionsResponse | null,
  }),
  getters: {
    /** EntitiesViewのUI誘導状態: 取得中=loading・一覧保持済み=result(0件の成功も
     * 含む)・取得失敗=error・未取得=not_fetched */
    entitiesView(): 'loading' | 'result' | 'error' | 'not_fetched' {
      if (this.loading) return 'loading'
      if (this.lastError) return 'error'
      if (this.fetched) return 'result'
      return 'not_fetched'
    },
  },
  actions: {
    /** 検索語の変更(絞り込み変更後は先頭pageから表示するためpageを1へ戻す) */
    setQuery(query: string): void {
      this.query = query
      this.page = 1
    },
    /** category絞り込みの変更(nullで解除。pageを1へ戻す) */
    setCategory(category: string | null): void {
      this.category = category
      this.page = 1
    },
    /** 表示pageの指定 */
    setPage(page: number): void {
      this.page = page
    },
    /** 一覧の取得(現在の絞り込み状態をURL queryへ反映。失敗時はitemsを空へ戻し
     * lastErrorを保持) */
    async fetchEntities(): Promise<void> {
      this.loading = true
      const result = await fetchEntities({
        query: this.query,
        category: this.category ?? undefined,
        page: this.page,
      })
      if (result.ok) {
        this.items = result.data.entities
        this.page = result.data.page
        this.totalPages = result.data.total_pages
        this.fetched = true
        this.lastError = null
      } else {
        this.items = []
        this.lastError = result.error
      }
      this.loading = false
    },
    /** 手動sync後のentities一覧再取得(sync storeのrefresh action内から呼ぶ。
     * sync.specの契約=POST /api/sync成功後は絞り込みparam無しのGET /api/entities
     * で再取得するため絞り込み状態を初期へ戻す) */
    async refetchEntities(): Promise<void> {
      this.query = ''
      this.category = null
      this.page = 1
      this.loading = true
      const result = await fetchEntities()
      if (result.ok) {
        this.items = result.data.entities
        this.page = result.data.page
        this.totalPages = result.data.total_pages
        this.fetched = true
        this.lastError = null
      } else {
        this.items = []
        this.lastError = result.error
      }
      this.loading = false
    },
    /** 詳細の取得(version_list付き。selectedUuid=uuid。不在404はdetailErrorで示し
     * detailは保持しない) */
    async fetchEntityDetail(uuid: string): Promise<void> {
      const result = await fetchEntityDetail(uuid)
      if (result.ok) {
        this.selectedUuid = uuid
        this.detail = result.data
        this.detailError = null
        return
      }
      this.detail = null
      this.detailError = result.error
    },
    /** 版一覧の取得(state付き。失敗時はversionsを空へ戻しdetailErrorで示す) */
    async fetchEntityVersions(uuid: string): Promise<void> {
      const result = await fetchEntityVersions(uuid)
      if (result.ok) {
        this.selectedUuid = uuid
        this.versions = result.data
        this.detailError = null
        return
      }
      this.versions = null
      this.detailError = result.error
    },
  },
})
