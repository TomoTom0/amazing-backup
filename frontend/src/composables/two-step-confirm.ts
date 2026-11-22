import { readonly, ref } from 'vue'

/**
 * 2段階確認の状態logic(TASK-17設計8章の新規部品「2段階確認」の状態部分。
 * 6章: browser dialog禁止のためinlineの2段階button=「実行」→同位置が「確認して実行」
 * (btn-primary)へ置換+escapable)。
 *
 * - 初期は非armed(部品は通常labelのbuttonとして振る舞う)
 * - 1回目の操作要求(advance)でarmedへ遷移し'armed'を返す(部品は確認labelへ置換)
 * - armed状態の2回目の操作要求が実行確定で'confirmed'を返しarmedは解除される
 *   (呼び出し側はこの戻り値で書込を実行する)
 * - escapable: Esc keydown(onKeydown)または明示cancel(blur等)でarmedを解除する
 */
export type TwoStepConfirmState = {
  /** armed状態の読取専用view(部品の表示切替に使う) */
  armed: Readonly<ReturnType<typeof ref<boolean>>>
  /** 操作要求(click相当)。非armed→'armed'・armed→'confirmed'(確定=armed解除) */
  advance: () => 'armed' | 'confirmed'
  /** 解除(Esc・blur等の明示取消。非armed時は状態不変) */
  cancel: () => void
  /** keydown handler(Escで解除。部品のbuttonへ@keydownで結合する。引数は
   * key文字列を持つeventであれば何でも受け付ける=DOM型に依存しない) */
  onKeydown: (event: { key: string }) => void
}

/** 2段階確認の状態を作る(部品ごとに1 instance) */
export function useTwoStepConfirm(): TwoStepConfirmState {
  const armed = ref(false)
  const advance = (): 'armed' | 'confirmed' => {
    if (!armed.value) {
      armed.value = true
      return 'armed'
    }
    armed.value = false
    return 'confirmed'
  }
  const cancel = (): void => {
    armed.value = false
  }
  const onKeydown = (event: { key: string }): void => {
    if (event.key === 'Escape') {
      cancel()
    }
  }
  return { armed: readonly(armed), advance, cancel, onKeydown }
}
