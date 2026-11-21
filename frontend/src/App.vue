<script setup lang="ts">
// TASK-17設計2.0: navは5タブ(ホーム・バックアップ・転送・設定・管理)へ拡張
// (旧4link(/・/entities・/pending・/diff)の再編。TASK-16 nav部品の踏襲)。
// 実行中jobがある間の転送tabへのstatus dotはW4のjob機構実装時に追加。
</script>

<template>
  <header class="app-header">
    <h1>amazing-backup</h1>
    <nav>
      <ul class="app-nav">
        <li><router-link to="/">ホーム</router-link></li>
        <li><router-link to="/backups">バックアップ</router-link></li>
        <li><router-link to="/transfer">転送</router-link></li>
        <li><router-link to="/settings">設定</router-link></li>
        <li><router-link to="/admin">管理</router-link></li>
      </ul>
    </nav>
  </header>
  <main class="app-main">
    <RouterView />
  </main>
</template>
