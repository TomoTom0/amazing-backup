<script setup lang="ts">
import { onMounted } from 'vue'
import { useSyncStore } from '../stores/sync'
import SyncStatus from '../components/SyncStatus.vue'

const sync = useSyncStore()
onMounted(() => {
  void sync.fetchHealth()
  // sync状態(保護API。P2〜P3はfail-closed 503のためlastErrorに閉鎖状態を保持する)
  void sync.fetchSyncState()
})
</script>

<template>
  <SyncStatus
    :health="sync.health"
    :message="sync.message"
    :sync-fetch="sync.syncFetch"
    :sync-status="sync.syncStatus"
    :sync-scope="sync.syncScope"
    :sync-fetched-at="sync.syncFetchedAt"
    :sync-entry-count="sync.syncEntryCount"
    :last-error="sync.lastError"
  />
  <section class="card">
    <h2>データ照会</h2>
    <p class="muted">
      entity一覧等の照会画面は後続phaseで提供されます。passkey認証後も当面はCLI
      (list・query)をご利用ください(未認証の場合はlogin画面へ誘導されます)。
    </p>
    <p>
      <router-link to="/pending">アップロード前カタログ(pending)</router-link>
    </p>
  </section>
</template>
