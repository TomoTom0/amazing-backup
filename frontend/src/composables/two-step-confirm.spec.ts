import { describe, expect, it } from 'vitest'
import { useTwoStepConfirm } from './two-step-confirm'

// 2段階確認部品の状態logic(TASK-17設計8章: button「実行」→同位置が「確認して実行」
// (btn-primary)へ置換+escapable。dialog不使用)・9章W3(TASK-22)。
// 部品の実描画は実browser観測(条件書frontend.tomlのexcluded参照)のため、
// ここではcomposableの状態遷移のみを担保する。

const escKeydown = (): { key: string } => ({ key: 'Escape' })

// [covers:frontend.two-step-confirm-state]
describe('useTwoStepConfirm(2段階確認の状態)', () => {
  it('初期は非armed・1回目advanceはarmedへ遷移し"armed"を返す', () => {
    const confirm = useTwoStepConfirm()
    expect(confirm.armed.value).toBe(false)
    expect(confirm.advance()).toBe('armed')
    expect(confirm.armed.value).toBe(true)
  })

  it('armed状態の2回目advanceは"confirmed"を返しarmedは解除される', () => {
    const confirm = useTwoStepConfirm()
    confirm.advance()
    expect(confirm.advance()).toBe('confirmed')
    expect(confirm.armed.value).toBe(false)
  })

  it('Esc keydown(armed時)はarmedを解除する(escapable)', () => {
    const confirm = useTwoStepConfirm()
    confirm.advance()
    confirm.onKeydown(escKeydown())
    expect(confirm.armed.value).toBe(false)
  })

  it('非armed時のEsc keydown・cancelは状態不変・確定後の再操作は再度armedから始まる', () => {
    const confirm = useTwoStepConfirm()
    confirm.onKeydown(escKeydown())
    expect(confirm.armed.value).toBe(false)
    confirm.cancel()
    expect(confirm.armed.value).toBe(false)
    // 確定→再操作は新たな2段階の1回目として振る舞う
    confirm.advance()
    confirm.advance()
    expect(confirm.advance()).toBe('armed')
  })
})
