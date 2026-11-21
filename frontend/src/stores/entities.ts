import { defineStore } from 'pinia'
import {
  fetchEntities,
  fetchEntityDetail,
  fetchEntityVersions,
  putEntityTags,
  renameEntity as renameEntityRequest,
  updateEntityLocation,
  type ApiError,
  type ApiResult,
  type EntitiesResponse,
  type EntityDetail,
  type EntitySummary,
  type EntityVersionsResponse,
} from '../api/client'
import { useTagsStore } from './tags'

/** 一覧のビュー種別(TASK-17設計2.2: flat=一覧・folder=folder構造・tag=tag選択・
 *  collection=collection選択。検索box・pagerは3ビュー共通で動く) */
export type EntitiesViewKind = 'flat' | 'folder' | 'tag' | 'collection'

/**
 * entities store(TASK-9設計7章=EntitiesView(一覧・検索・絞込・paging)・
 * EntityDetailView・コードレビュー指摘高4+W2(TASK-17設計2.2: ビュー切替状態・
 * label/tag/collection絞り込み・tag編集)。一覧・絞り込み状態とUI誘導状態
 * (entitiesView getter)・詳細/versions状態を保持する。取得失敗時はitemsを空へ
 * 戻しlastErrorを保持する(空dataを表示しない=fail-closedと共通)。
 */
export const useEntitiesStore = defineStore('entities', {
  state: () => ({
    /** entity一覧(未取得・取得失敗時は空) */
    items: [] as EntitySummary[],
    /** 検索語(空文字列=絞り込み無し。3ビュー共通) */
    query: '',
    /** category絞り込み(null=絞り込み無し。3ビュー共通) */
    category: null as string | null,
    /** label(folder)絞り込み(null=絞り込み無し。folderビューの選択folder=
     *  既有label=絞り込みで成立するため専用paramは設けない) */
    labelFilter: null as string | null,
    /** tag絞り込み(null=絞り込み無し。tagビューの選択tag) */
    tagFilter: null as string | null,
    /** tag無し絞り込み(true=untagged=1でtag無しentityのみ。tagFilterとの同時指定は
     *  server側400のためsetTagFilter/setUntaggedFilterで相互に排他する) */
    untaggedOnly: false,
    /** collection絞り込み(null=絞り込み無し。collectionビューの選択collection
     *  id。絞り込み直列化時に文字列化する) */
    collectionFilter: null as number | null,
    /** 一覧のビュー種別(folder/tag/collectionは選択系絞り込みを持つ) */
    view: 'flat' as EntitiesViewKind,
    /** 現在page(1始点) */
    page: 1,
    totalPages: 1,
    loading: false,
    /** 一覧取得を1回以上成功させたか(0件の成功もresult表示のためのflag) */
    fetched: false,
    lastError: null as ApiError | null,
    /** 選択中entity(EntityDetailView表示対象) */
    selectedUuid: null as string | null,
    /** 詳細(version_list+collections付き。不在404時は保持しない) */
    detail: null as EntityDetail | null,
    /** 詳細取得のerror(versions取得のerrorとは分離=並行取得で片方の成功が
     * もう片方の失敗を隠さない) */
    detailError: null as ApiError | null,
    /** 版一覧(state=現行/noncurrent付き) */
    versions: null as EntityVersionsResponse | null,
    /** versions取得のerror(detail取得のerrorとは分離) */
    versionsError: null as ApiError | null,
    /** tag編集(PUT tags)のerror(一覧取得のerrorとは分離=編集formの失敗表示用) */
    lastTagEditError: null as ApiError | null,
    /** tag編集(PUT tags)の実行中状態(保存応答待ち中の後続PUTが同じ旧配列から
     *  作られて先行編集を消すのを防ぐ=Viewは本値でtag編集UIを無効化) */
    tagEditSaving: false,
    /** title編集(POST rename)のerror(編集formの失敗表示用・W3=TASK-22) */
    lastRenameError: null as ApiError | null,
    /** title編集(POST rename)の実行中状態( Viewは本値でtitle編集UIを無効化) */
    renameSaving: false,
    /** title編集の重複警告(rename応答のwarning。重複無し成功でnullへ戻す) */
    lastRenameWarning: null as string | null,
    /** 所在編集(POST location)のerror(編集formの失敗表示用・W3=TASK-22) */
    lastLocationError: null as ApiError | null,
    /** 所在編集(POST location)の実行中状態(Viewは本値で所在編集UIを無効化) */
    locationSaving: false,
    /** 一覧取得の要求世代(fetchEntities/refetchEntitiesの開始時に増加)。旧要求の
     *  応答の後着は不採用とする(連続選択で並行GETの旧応答が採用され選択状態と
     *  一覧が食い違うのを防ぐ=詳細系detailEpochと同一構成) */
    listRequest: 0,
    /** 詳細・versionsの失効世代(refetchEntitiesで増加)。失効前に開始した取得の
     * 応答は不採用とする(失効後への旧内容再格納防止)。EntityDetailViewは本値の
     * 変化をwatchして表示中の詳細を再取得する */
    detailEpoch: 0,
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
    /** label(folder)絞り込みの変更(nullで解除。pageを1へ戻す) */
    setLabelFilter(label: string | null): void {
      this.labelFilter = label
      this.page = 1
    },
    /** tag絞り込みの変更(nullで解除=tag無し選択。pageを1へ戻す。tag選択時は
     *  untaggedOnlyをクリアする=tag=とuntagged=の相互排他) */
    setTagFilter(tag: string | null): void {
      this.tagFilter = tag
      this.untaggedOnly = false
      this.page = 1
    },
    /** tag無し絞り込みの選択(untagged=1でtag無しentityのみ。tagFilterをクリア
     *  しpageを1へ戻す) */
    setUntaggedFilter(): void {
      this.untaggedOnly = true
      this.tagFilter = null
      this.page = 1
    },
    /** collection絞り込みの変更(nullで解除。pageを1へ戻す) */
    setCollectionFilter(id: number | null): void {
      this.collectionFilter = id
      this.page = 1
    },
    /** ビュー切替(flat/folder/tag/collection)。選択系絞り込み(tag/collection/label)
     * はビュー切替でクリアしpageを1へ戻す。検索box(query)・category絞り込みは
     * 3ビュー共通で動くため維持する */
    setView(view: EntitiesViewKind): void {
      this.view = view
      this.tagFilter = null
      this.untaggedOnly = false
      this.collectionFilter = null
      this.labelFilter = null
      this.page = 1
    },
    /** 表示pageの指定 */
    setPage(page: number): void {
      this.page = page
    },
    /** 一覧の取得(現在の絞り込み状態をURL queryへ反映。失敗時はitemsを空へ戻し
     * lastErrorを保持。開始時に要求世代を進め、完了時点で最新でない応答は
     * 採用しない=並行取得の旧応答後着の不採用) */
    async fetchEntities(): Promise<void> {
      const request = ++this.listRequest
      this.loading = true
      const result = await fetchEntities({
        query: this.query,
        category: this.category ?? undefined,
        label: this.labelFilter ?? undefined,
        tag: this.tagFilter ?? undefined,
        collection:
          this.collectionFilter === null ? undefined : String(this.collectionFilter),
        untagged: this.untaggedOnly || undefined,
        page: this.page,
      })
      if (request !== this.listRequest) return
      this.applyListResult(result)
    },
    /** 一覧取得応答のstate反映(最新要求の応答からのみ呼ぶ) */
    applyListResult(result: ApiResult<EntitiesResponse>): void {
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
    /** 検索語とcategory絞り込みの同時適用(EntitiesViewのfilter formのsubmitから
     * 呼ぶ=1回のsubmitで両入力を反映し再取得も1回にまとめる) */
    async applyFilters(query: string, category: string | null): Promise<void> {
      this.setQuery(query)
      this.setCategory(category)
      await this.fetchEntities()
    },
    /** 手動sync後のentities一覧再取得(sync storeのrefresh action内から呼ぶ。
     * sync.specの契約=POST /api/sync成功後は絞り込みparam無しのGET /api/entities
     * で再取得するため絞り込み状態を初期へ戻す。syncで索引が置き換わるため
     * 詳細・versionsの保持内容も失効させる(旧locations/pinsと新versions一覧の
     * 混在を構造的に排除=詳細を開く際に再取得させる) */
    async refetchEntities(): Promise<void> {
      const request = ++this.listRequest
      this.query = ''
      this.category = null
      this.labelFilter = null
      this.tagFilter = null
      this.untaggedOnly = false
      this.collectionFilter = null
      this.view = 'flat'
      this.page = 1
      this.detailEpoch += 1
      this.selectedUuid = null
      this.detail = null
      this.detailError = null
      this.versions = null
      this.versionsError = null
      this.loading = true
      const result = await fetchEntities()
      if (request !== this.listRequest) return
      this.applyListResult(result)
    },
    /** 詳細の取得(version_list付き。selectedUuid=uuid。不在404はdetailErrorで示し
     * detailは保持しない。失効(sync後再取得)をまたいだ応答は不採用とする) */
    async fetchEntityDetail(uuid: string): Promise<void> {
      const epoch = this.detailEpoch
      const result = await fetchEntityDetail(uuid)
      if (epoch !== this.detailEpoch) return
      if (result.ok) {
        this.selectedUuid = uuid
        this.detail = result.data
        this.detailError = null
        return
      }
      this.detail = null
      this.detailError = result.error
    },
    /** 版一覧の取得(state付き。失敗時はversionsを空へ戻しversionsErrorで示す=
     * detailErrorとは分離し片方の失敗がもう片方の成功で消えない。失効をまたいだ
     * 応答は不採用とする) */
    async fetchEntityVersions(uuid: string): Promise<void> {
      const epoch = this.detailEpoch
      const result = await fetchEntityVersions(uuid)
      if (epoch !== this.detailEpoch) return
      if (result.ok) {
        this.selectedUuid = uuid
        this.versions = result.data
        this.versionsError = null
        return
      }
      this.versions = null
      this.versionsError = result.error
    },
    /** tag編集(PUT /api/entities/:uuid/tagsでtag一覧を丸ごと置換)。応答は操作後の
     *  対象状態(読取系と同一schema=design doc 3.1方針)のため対象詳細の再GETは行わ
     *  ず応答でdetailを更新する。失敗(422 validation_failed等)はlastTagEditErrorで
     *  示しdetailは旧状態のまま(成功でlastTagEditErrorはクリア)。実行中は
     *  tagEditSaving=true(Viewはtag編集UIを無効化)。成功時は一覧(tag編集は行の
     *  tagsに影響するため取得済みなら現在の絞り込みで再取得)とtag選択肢(取得済み
     *  なら)も再取得する=旧tags・旧絞り込み結果が一覧へ残らない */
    async updateTags(uuid: string, tags: string[]): Promise<void> {
      this.tagEditSaving = true
      const result = await putEntityTags(uuid, tags)
      if (result.ok) {
        if (this.selectedUuid === uuid) {
          this.detail = result.data
        }
        this.lastTagEditError = null
        this.tagEditSaving = false
        if (this.fetched) {
          await this.fetchEntities()
        }
        const tagsStore = useTagsStore()
        if (tagsStore.fetchState === 'ok') {
          await tagsStore.fetchTags()
        }
        return
      }
      this.lastTagEditError = result.error
      this.tagEditSaving = false
    },
    /** title編集(POST /api/entities/:uuid/rename。W3=TASK-22)。応答は操作後の
     *  対象状態(+title重複時のみwarning=許容の警告)のため対象詳細の再GETは行わず
     *  応答でdetailを更新する。失敗(422等)はlastRenameErrorで示しdetailは旧状態の
     *  まま(成功でクリア)。実行中はrenameSaving=true(Viewはtitle編集UIを無効化)。
     *  成功時はtitle変更が一覧表示に影響するため取得済みなら現在の絞り込みで
     *  一覧を再取得する=旧titleが一覧へ残らない */
    async renameEntity(uuid: string, title: string): Promise<void> {
      this.renameSaving = true
      const result = await renameEntityRequest(uuid, title)
      if (result.ok) {
        if (this.selectedUuid === uuid) {
          this.detail = result.data
        }
        this.lastRenameError = null
        this.lastRenameWarning = result.data.warning ?? null
        this.renameSaving = false
        if (this.fetched) {
          await this.fetchEntities()
        }
        return
      }
      this.lastRenameError = result.error
      this.renameSaving = false
    },
    /** 所在の分類編集(POST /api/entities/:uuid/location。W3=TASK-22)。応答は操作後の
     *  対象状態のため対象詳細の再GETは行わず応答でdetailを更新する。失敗(404/422)は
     *  lastLocationErrorで示しdetailは旧状態のまま(成功でクリア)。実行中は
     *  locationSaving=true。成功時はfolder一覧ビューに影響するため取得済みなら
     *  一覧を再取得する */
    async updateLocation(
      uuid: string,
      sourcePath: string,
      category: string,
      label: string,
    ): Promise<void> {
      this.locationSaving = true
      const result = await updateEntityLocation(uuid, sourcePath, category, label)
      if (result.ok) {
        if (this.selectedUuid === uuid) {
          this.detail = result.data
        }
        this.lastLocationError = null
        this.locationSaving = false
        if (this.fetched) {
          await this.fetchEntities()
        }
        return
      }
      this.lastLocationError = result.error
      this.locationSaving = false
    },
  },
})
