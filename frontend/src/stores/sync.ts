import { defineStore } from 'pinia'
import {
  apiGet,
  fetchSyncState,
  isHealthResponse,
  type ApiError,
} from '../api/client'

/** liveness(health)の表示状態 */
export type HealthState = 'checking' | 'ok' | 'error'

/** sync状態の取得状態(未取得=fresh未実行とは無関係のUI状態) */
export type SyncFetchState = 'idle' | 'loading' | 'ok' | 'error'

type SyncState = {
  health: HealthState
  message: string
  lastError: ApiError | null
  /** sync状態(GET /api/sync)の取得状態 */
  syncFetch: SyncFetchState
  /** sync_stateの保存値(null=未取得・fetch失敗時は前値を残さない) */
  syncStatus: string | null
  syncScope: string
  syncFetchedAt: string
  syncLastVerifiedAt: string
  syncEntryCount: number
}

/**
 * sync状態store(TASK-9設計7章・D10: liveness表示+staleness表示)。
 * P2〜P3は保護APIがfail-closed(503 configuration)のためfetchSyncStateは
 * error扱いになり、lastErrorに閉鎖状態を保持してdata表示を行わない
 * (frontend.fail-closed-protected-api-handling)。
 */
export const useSyncStore = defineStore('sync', {
  state: (): SyncState => ({
    health: 'checking',
    message: '',
    lastError: null,
    syncFetch: 'idle',
    syncStatus: null,
    syncScope: '',
    syncFetchedAt: '',
    syncLastVerifiedAt: '',
    syncEntryCount: 0,
  }),
  actions: {
    async fetchHealth(): Promise<void> {
      const result = await apiGet('/api/health', isHealthResponse)
      if (result.ok) {
        this.health = 'ok'
        this.message = result.data.status
        this.lastError = null
        return
      }
      this.health = 'error'
      this.message = ''
      this.lastError = result.error
    },
    /** sync状態の取得(失敗時はsyncStatusをnullに戻しlastErrorで閉鎖・障害を示す) */
    async fetchSyncState(): Promise<void> {
      this.syncFetch = 'loading'
      const result = await fetchSyncState()
      if (result.ok) {
        this.syncFetch = 'ok'
        this.syncStatus = result.data.status
        this.syncScope = result.data.scope
        this.syncFetchedAt = result.data.fetched_at
        this.syncLastVerifiedAt = result.data.last_verified_at
        this.syncEntryCount = result.data.entry_count
        return
      }
      this.syncFetch = 'error'
      this.syncStatus = null
      this.syncScope = ''
      this.syncFetchedAt = ''
      this.syncLastVerifiedAt = ''
      this.syncEntryCount = 0
      this.lastError = result.error
    },
  },
})
