<script setup lang="ts">
import { onMounted } from 'vue'
import { useSyncStore } from '../stores/sync'
import SyncStatus from '../components/SyncStatus.vue'

const sync = useSyncStore()
onMounted(() => {
  void sync.fetchHealth()
})
</script>

<template>
  <SyncStatus :health="sync.health" :message="sync.message" />
  <section class="card">
    <h2>データ照会</h2>
    <p class="muted">
      entity一覧等の保護APIはpasskey認証の構成完了まで閉鎖されています(P2〜P3はfail-closed)。
      照会は当面CLI(list・query)を利用してください。
    </p>
  </section>
</template>
