import { describe, expect, it } from 'vitest'
import { isApiErrorResponse, isHealthResponse } from './client'

// vitest最小setup(TASK-9 P2。P3+で本格的に使用)
describe('api型ガード', () => {
  it('error body形式を判定する', () => {
    expect(
      isApiErrorResponse({ error: { code: 'configuration', message: '閉鎖中' } }),
    ).toBe(true)
    expect(isApiErrorResponse({ error: { code: 1, message: 'x' } })).toBe(false)
    expect(isApiErrorResponse({ error: { code: 'a' } })).toBe(false)
    expect(isApiErrorResponse({})).toBe(false)
    expect(isApiErrorResponse(null)).toBe(false)
  })

  it('health応答形式を判定する', () => {
    expect(isHealthResponse({ status: 'ok' })).toBe(true)
    expect(isHealthResponse({ status: 1 })).toBe(false)
    expect(isHealthResponse('ok')).toBe(false)
  })
})
