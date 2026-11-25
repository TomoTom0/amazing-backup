<script setup lang="ts">
import { watch } from 'vue'
import { useAuthStore } from './stores/auth'
import { useJobsStore } from './stores/jobs'

// TASK-17設計2.0: navは5タブ(ホーム・バックアップ・転送・設定・管理)へ拡張
// (旧4link(/・/entities・/pending・/diff)の再編。TASK-16 nav部品の踏襲)。
// W4(TASK-27・設計2.0): 実行中jobがある間は転送tabにstatus dot(checking色)を
// 付す(進行の気づき)。dotのdata源はjobs storeのpolling(設計5.3の転送タブ・
// ホーム表示中の追従に加え・他tab表示中の気づきのためにApp.vueが認証済みの間
// pollingを保持する=参照カウントで転送タブ・ホームのpollingと独立)。
const auth = useAuthStore()
const jobs = useJobsStore()

// 認証済みの間だけnav dot用のpollingを保持する(未認証時の401連発を避ける)
watch(
  () => auth.authenticated,
  (authenticated) => {
    if (authenticated) {
      jobs.startPolling()
    } else {
      jobs.stopPolling()
    }
  },
  { immediate: true },
)
</script>

<template>
  <header class="app-header">
    <h1>amazing-backup</h1>
    <nav>
      <ul class="app-nav">
        <li><router-link to="/">ホーム</router-link></li>
        <li><router-link to="/backups">バックアップ</router-link></li>
        <li>
          <router-link to="/transfer">
            転送
            <span
              v-if="jobs.hasActiveJobs"
              class="job-status st-running nav-job-dot"
              :aria-label="`実行中のjobが${jobs.activeJobs.length}件あります`"
            ></span>
          </router-link>
        </li>
        <li><router-link to="/settings">設定</router-link></li>
        <li><router-link to="/admin">管理</router-link></li>
      </ul>
    </nav>
  </header>
  <main class="app-main">
    <RouterView />
  </main>
</template>
