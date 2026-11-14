<script setup lang="ts">
// TASK-17設計2.5・9章W1: 管理タブの枠view。秘密鍵・認証credential・操作履歴・
// backup実行履歴・転送履歴の取り扱いはW6(鍵・認証管理)〜W7(履歴)で実装するため
// W1では案内のみ。
</script>

<template>
  <section class="card">
    <h2>管理</h2>
    <p class="muted">
      秘密鍵(recipient)・認証credential・操作履歴・backup実行履歴・転送履歴はこの
      画面で取り扱います(各種管理機能はまだ用意されていません)。
    </p>
    <p class="muted">
      現時点はCLIのmanage auth・keys系操作をご利用ください。
    </p>
  </section>
</template>
