<script setup lang="ts">
// TASK-17設計2.3・9章W1: 転送タブの枠view。取得form・upload form・job一覧は
// W4(job機構+get)〜W5(push job)で実装するためW1では案内のみ。
</script>

<template>
  <section class="card">
    <h2>転送</h2>
    <p class="muted">
      fileの取得・uploadと長時間jobの実行状況はこの画面で扱います(取得・uploadの
      実行機能はまだ用意されていません)。
    </p>
    <p class="muted">
      実行中・待機中のjobは現在ありません。backup対象の追加はCLIのpushを、fileの
      取得はCLIのgetを引き続きご利用ください。
    </p>
  </section>
</template>
