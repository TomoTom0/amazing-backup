<script setup lang="ts">
import { onMounted } from 'vue'
import { usePendingStore } from '../stores/pending'

// TASK-9設計7章(PendingView=未backup/changed/unknown一覧+scan)・12章P5。
// 一覧のUI状態はpending store(items・counts getter)が持ち、scan実行はrunScan
// action(POST後に一覧再取得)。実描画の観測は実browser(条件書frontend.tomlの
// excluded参照)。

const pending = usePendingStore()

onMounted(() => {
  void pending.fetchPending()
})

const fmtSize = (size: number): string => {
  if (size >= 1024 * 1024 * 1024) return `${(size / (1024 * 1024 * 1024)).toFixed(1)} GB`
  if (size >= 1024 * 1024) return `${(size / (1024 * 1024)).toFixed(1)} MB`
  if (size >= 1024) return `${(size / 1024).toFixed(1)} KB`
  return `${size} B`
}

// TASK-16設計3章(pending status列): status -> 色分けclass(visual design。
// st-* class selector単層でCSS側は解釈=構造位置selectorに依存しない)
const statusClass = (status: string): string =>
  status === 'unbacked' || status === 'changed' || status === 'ok'
    ? `st-${status}`
    : 'st-unknown'
</script>

<template>
  <section class="card">
    <h2>アップロード前カタログ</h2>
    <p class="muted">
      scan対象はpipeline.toml [catalog.scan]の登録root配下(local FS走査・
      symlinkは辿りません)。分類は未backup(unbacked)・変更あり(changed)・
      backup済み(ok)・確認不能(unknown)の4状態です。
    </p>

    <div class="scan-actions">
      <button type="button" class="btn-primary" :disabled="pending.scanning" @click="pending.runScan()">
        {{ pending.scanning ? 'scan実行中...' : 'scan実行' }}
      </button>
      <span v-if="pending.counts.unbacked + pending.counts.changed > 0" class="muted">
        未backup {{ pending.counts.unbacked }}件・変更 {{ pending.counts.changed }}件
      </span>
    </div>

    <p v-if="pending.lastError" class="message">
      取得に失敗しました({{ pending.lastError.code }})。空のdataは表示していません。
    </p>
    <p v-if="pending.lastScanError" class="message">
      直前のscan実行に失敗しました({{ pending.lastScanError.code }})。
      以下は最後に取得できた一覧です。
    </p>

    <table v-if="pending.items.length > 0" class="pending-table">
      <thead>
        <tr>
          <th>status</th>
          <th>path</th>
          <th>size</th>
          <th>mtime</th>
        </tr>
      </thead>
      <tbody>
        <tr v-for="item in pending.items" :key="`${item.source_root}:${item.path}`">
          <td :class="statusClass(item.status)">{{ item.status }}</td>
          <td class="mono">{{ item.path }}</td>
          <td class="mono">{{ fmtSize(item.size) }}</td>
          <td class="mono">{{ item.mtime }}</td>
        </tr>
      </tbody>
    </table>
    <p v-else-if="!pending.lastError" class="muted">
      対象0件(pipeline.toml [catalog.scan]の登録とscan実行をご確認ください)。
    </p>
  </section>
</template>
