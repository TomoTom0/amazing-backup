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
      照会画面の一覧です(passkey認証後に閲覧できます。未認証の場合はlogin画面へ誘導
      されます)。
    </p>
    <p>
      <router-link to="/entities">entity一覧(検索・絞込・sync状態+refresh)</router-link>
    </p>
    <p>
      <router-link to="/pending">アップロード前カタログ(pending)</router-link>
    </p>
    <p>
      <router-link to="/diff">完全性突合(diff)</router-link>
    </p>
  </section>
</template>
