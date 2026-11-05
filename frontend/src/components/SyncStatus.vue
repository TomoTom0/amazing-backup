<script setup lang="ts">
import type { HealthState } from '../stores/sync'

defineProps<{
  health: HealthState
  message: string
}>()
</script>

<template>
  <section class="card">
    <h2>server状態</h2>
    <p v-if="health === 'checking'" class="status-checking">確認中...</p>
    <p v-else-if="health === 'ok'" class="status-ok">起動中(status: {{ message }})</p>
    <p v-else class="status-error">serverに接続できません(serveが起動しているか確認してください)</p>
  </section>
</template>
