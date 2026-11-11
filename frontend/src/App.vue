<script setup lang="ts">
// TASK-9設計7章: 最小構成(TASK-9 P2)。UI library無し・素のVue3+TS+最小CSS。
</script>

<template>
  <header class="app-header">
    <h1>amazing-backup</h1>
    <nav>
      <ul class="app-nav">
        <li><router-link to="/">home</router-link></li>
        <li><router-link to="/entities">一覧</router-link></li>
        <li><router-link to="/pending">pending</router-link></li>
        <li><router-link to="/diff">diff</router-link></li>
      </ul>
    </nav>
  </header>
  <main class="app-main">
    <RouterView />
  </main>
</template>
