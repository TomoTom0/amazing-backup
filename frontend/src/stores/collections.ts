import { defineStore } from 'pinia'
import {
  addCollectionItems,
  createCollection,
  deleteCollection,
  fetchCollectionDetail,
  fetchCollections,
  removeCollectionItem,
  renameCollection,
  type ApiError,
  type CollectionItem,
} from '../api/client'
import { useEntitiesStore } from './entities'
import type { OptionsFetchState } from './tags'

/**
 * collections store(TASK-17設計2.2・9章W2): collectionビュー=collection選択+その
 * 構成entity一覧の表示基盤。一覧(items)と選択collectionの構成uuid一覧
 * (detailId・detailUuids)を保持する。取得失敗時は対象stateを空へ戻しlastErrorを
 * 保持する(空dataを表示しない)。
 * W2レビュー指摘高3対応(TASK-19): collection作成・改名・削除・構成追加/除去の
 * 書込actionsを持つ(書込API契約は2026-11-14確定分。成功時は一覧を再取得し構成state
 * を操作後の応答で更新する。失敗はlastWriteErrorで保持し実行中はwriting)。
 */
export const useCollectionsStore = defineStore('collections', {
  state: () => ({
    /** collection一覧(未取得・取得失敗時は空) */
    items: [] as CollectionItem[],
    /** 選択中collectionのid(未選択・失敗時はnull) */
    detailId: null as number | null,
    /** 選択中collectionの構成uuid一覧(未取得・失敗時は空) */
    detailUuids: [] as string[],
    /** 取得状態(失敗後もfetchCollectionsの再呼出で再取得できる=Viewの再試行button基盤) */
    fetchState: 'not_fetched' as OptionsFetchState,
    lastError: null as ApiError | null,
    /** 直近の書込(create/rename/delete/items)のerror(成功でクリア。取得系
     *  lastErrorとは別状態=P5 lastScanError等と同一構成) */
    lastWriteError: null as ApiError | null,
    /** 書込実行中状態(Viewは本値で書込UIを無効化=二重送信防止) */
    writing: false,
  }),
  actions: {
    /** collection一覧の取得(失敗時はitemsを空へ戻しlastErrorを保持) */
    async fetchCollections(): Promise<void> {
      const result = await fetchCollections()
      if (result.ok) {
        this.items = result.data.collections
        this.fetchState = 'ok'
        this.lastError = null
        return
      }
      this.items = []
      this.fetchState = 'error'
      this.lastError = result.error
    },
    /** 選択collectionの構成uuid一覧取得(応答は詳細schema{collection:{id,name,
     *  uuids}}のためuuidsはcollection経由で取り出す。不在idの404 not_foundは
     *  error扱い。失敗時はdetailUuidsを空へ戻す) */
    async fetchCollectionDetail(id: number): Promise<void> {
      const result = await fetchCollectionDetail(id)
      if (result.ok) {
        this.detailId = result.data.collection.id
        this.detailUuids = result.data.collection.uuids
        this.lastError = null
        return
      }
      this.detailId = null
      this.detailUuids = []
      this.lastError = result.error
    },
    /** collection作成(POST /api/collections)。成功時は構成stateへ保持し一覧を
     *  再取得したうえで作成されたcollectionを選択する(作成直後に構成編集へ
     *  着手できる導線=entities一覧も選択で再取得される) */
    async createCollection(name: string): Promise<boolean> {
      this.writing = true
      const result = await createCollection(name)
      this.writing = false
      if (!result.ok) {
        this.lastWriteError = result.error
        return false
      }
      this.lastWriteError = null
      this.detailId = result.data.collection.id
      this.detailUuids = result.data.collection.uuids
      await this.fetchCollections()
      const entities = useEntitiesStore()
      entities.setCollectionFilter(result.data.collection.id)
      await entities.fetchEntities()
      return true
    },
    /** collection改名(PUT /api/collections/:id)。成功時は一覧を再取得し選択中の
     *  構成stateを操作後の応答で更新する */
    async renameCollection(id: number, name: string): Promise<boolean> {
      this.writing = true
      const result = await renameCollection(id, name)
      this.writing = false
      if (!result.ok) {
        this.lastWriteError = result.error
        return false
      }
      this.lastWriteError = null
      if (this.detailId === id) {
        this.detailId = result.data.collection.id
        this.detailUuids = result.data.collection.uuids
      }
      await this.fetchCollections()
      return true
    },
    /** collection削除(DELETE /api/collections/:id)。成功時は一覧を再取得し構成state
     *  を失効させる。削除したのが選択中collectionの場合は選択を解除(絞り込み無し)
     *  してentities一覧を再取得する */
    async deleteCollection(id: number): Promise<boolean> {
      this.writing = true
      const result = await deleteCollection(id)
      this.writing = false
      if (!result.ok) {
        this.lastWriteError = result.error
        return false
      }
      this.lastWriteError = null
      if (this.detailId === id) {
        this.detailId = null
        this.detailUuids = []
      }
      await this.fetchCollections()
      const entities = useEntitiesStore()
      if (entities.collectionFilter === id) {
        entities.setCollectionFilter(null)
        await entities.fetchEntities()
      }
      return true
    },
    /** 構成へのentity追加(POST /api/collections/:id/items。body {uuids})。成功時は
     *  構成uuid一覧を操作後の応答で更新し一覧(entity_count)を再取得する */
    async addCollectionItems(id: number, uuids: string[]): Promise<boolean> {
      this.writing = true
      const result = await addCollectionItems(id, uuids)
      this.writing = false
      if (!result.ok) {
        this.lastWriteError = result.error
        return false
      }
      this.lastWriteError = null
      this.detailId = result.data.collection.id
      this.detailUuids = result.data.collection.uuids
      await this.fetchCollections()
      return true
    },
    /** 構成からのentity除去(DELETE /api/collections/:id/items/:uuid)。成功時は構成
     *  uuid一覧を操作後の応答で更新し一覧(entity_count)を再取得する */
    async removeCollectionItem(id: number, uuid: string): Promise<boolean> {
      this.writing = true
      const result = await removeCollectionItem(id, uuid)
      this.writing = false
      if (!result.ok) {
        this.lastWriteError = result.error
        return false
      }
      this.lastWriteError = null
      this.detailId = result.data.collection.id
      this.detailUuids = result.data.collection.uuids
      await this.fetchCollections()
      return true
    },
  },
})
