<script setup lang="ts">
// TASK-17設計2.4・9章W1: 設定タブの枠view。backup schedule・接続先・scan対象の
// 設定表示はW7(schedule・設定)で実装するためW1では案内のみ。UI更新挙動(自動更新
// 間隔)の設定項目もここへ置く(W1ではfrontend側の保持のみ=stores/ui-refresh.ts)。
</script>

<template>
  <section class="card">
    <h2>設定</h2>
    <p class="muted">
      backup schedule・接続先・scan対象・UI更新挙動(自動更新間隔)をこの画面で
      確認・設定できます(設定項目はまだ用意されていません)。
    </p>
    <p class="muted">
      現時点の設定変更はpipeline.toml・server起動時引数で行ってください。
    </p>
  </section>
</template>
