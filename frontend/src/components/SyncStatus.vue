<script setup lang="ts">
import type { ApiError } from '../api/client'
import type { HealthState, SyncFetchState } from '../stores/sync'

defineProps<{
  health: HealthState
  message: string
  syncFetch: SyncFetchState
  syncStatus: string | null
  syncScope: string
  syncFetchedAt: string
  syncEntryCount: number
  lastError: ApiError | null
}>()

/** sync状態の表示文案(status=fresh/stale/error/unknown) */
function syncStatusText(status: string | null): string {
  if (status === 'fresh') return 'fresh(最新)'
  if (status === 'stale') return 'stale(要refresh)'
  if (status === 'error') return 'error(同期失敗)'
  if (status === 'unknown') return 'unknown(未同期)'
  return 'unknown'
}

/** sync状態の色分け(TASK-16のstatus色設計: fresh=ok・stale=warn・error=error・unknown=checking) */
function syncStatusClass(status: string | null): string {
  if (status === 'fresh') return 'status-ok'
  if (status === 'stale') return 'status-warn'
  if (status === 'error') return 'status-error'
  return 'status-checking'
}
</script>

<template>
  <section class="card">
    <h2>server状態</h2>
    <p v-if="health === 'checking'" class="status-checking">確認中...</p>
    <p v-else-if="health === 'ok'" class="status-ok">起動中(status: {{ message }})</p>
    <p v-else class="status-error">serverに接続できません(serveが起動しているか確認してください)</p>

    <h3>sync状態</h3>
    <!-- fetch失敗時のerror guard: lastErrorを持ちdataを表示しない -->
    <p v-if="syncFetch === 'loading' || syncFetch === 'idle'" class="status-checking">
      確認中...
    </p>
    <template v-else-if="syncFetch === 'ok' && syncStatus !== null">
      <p :class="syncStatusClass(syncStatus)">状態: {{ syncStatusText(syncStatus) }}</p>
      <p class="muted">scope: {{ syncScope }}</p>
      <p class="muted">
        取得時刻: {{ syncFetchedAt === '' ? '未取得' : syncFetchedAt }}
        (entity数: {{ syncEntryCount }})
      </p>
    </template>
    <p v-else-if="lastError !== null && lastError.code === 'configuration'" class="status-error">
      sync状態は保護APIのためpasskey認証の構成完了まで閉鎖されています
    </p>
    <p v-else class="status-error">sync状態を取得できません(code: {{ lastError?.code }})</p>
  </section>
</template>
