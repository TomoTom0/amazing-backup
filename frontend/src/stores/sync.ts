import { defineStore } from 'pinia'
import { apiGet, isHealthResponse, type ApiError } from '../api/client'

/** liveness(health)の表示状態 */
export type HealthState = 'checking' | 'ok' | 'error'

type SyncState = {
  health: HealthState
  message: string
  lastError: ApiError | null
}

/**
 * sync状態store(TASK-9設計7章・D10)。
 * P2ではliveness(health)のみ。手動refresh(POST /api/sync)・staleness表示はP3+。
 */
export const useSyncStore = defineStore('sync', {
  state: (): SyncState => ({
    health: 'checking',
    message: '',
    lastError: null,
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
  },
})
