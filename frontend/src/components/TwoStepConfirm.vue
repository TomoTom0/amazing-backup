<script setup lang="ts">
import { useTwoStepConfirm } from '../composables/two-step-confirm'

// 2段階確認button(TASK-17設計8章: button「実行」→同位置が「確認して実行」
// (btn-primary)へ置換+escapable。dialog不使用)。状態logicはcomposable
// (useTwoStepConfirm)が担い、本部品は表示切替とconfirm eventの発火のみ。
// 確定clickでconfirm eventをemitする(1回目clickはarmedへ遷移するのみ)。
// blur・Escでarmedを解除する(誤armedの取り消し)。TASK-16のtoken
// (btn-primary/btn-quiet)を再利用する。

const props = withDefaults(
  defineProps<{
    /** 通常状態のlabel(例: 「名前変更」) */
    label: string
    /** armed状態のlabel(既定「確認して実行」) */
    confirmLabel?: string
    /** disabled(保存中等の操作不可状態) */
    disabled?: boolean
  }>(),
  { confirmLabel: '確認して実行', disabled: false },
)

const emit = defineEmits<{
  confirm: []
}>()

const { armed, advance, cancel, onKeydown } = useTwoStepConfirm()

const onClick = (): void => {
  if (props.disabled) return
  if (advance() === 'confirmed') {
    emit('confirm')
  }
}
</script>

<template>
  <button
    type="button"
    :class="armed ? 'btn-primary' : 'btn-quiet'"
    :disabled="disabled"
    @click="onClick"
    @keydown="onKeydown"
    @blur="cancel"
  >{{ armed ? confirmLabel : label }}</button>
</template>
