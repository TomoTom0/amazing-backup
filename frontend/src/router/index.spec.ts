import { describe, expect, it } from 'vitest'
import { routes } from './routes'

// TASK-17設計2.0・2.2・9章W1: nav 5タブ化(ホーム/・バックアップ/backups・転送/
// transfer・設定/settings・管理/admin)+既存4画面の再配置(一覧/詳細→/backups配下・
// pending/diff→/backupsのfilter view=query paramのため独立routeなし)。routes配列は
// router/index.tsから切り出したfrontend/src/router/routes.tsを参照する
// (=createWebHistoryがnode環境で動かないための観測seam。index.tsはroutesを
// importして組立てる)。W1実装前のため本fileのtestは実行failを許容(実装後に緑化)。
// nav link・filter viewの実描画・router guardの実navigationはcomponent実描画と
// 同様に実browser観測のため条件書frontend.tomlのexcluded参照。

// [covers:frontend.entities-routes-nav]
describe('routerの5タブroutes(TASK-17 W1再編)', () => {
  it('5タブ(/・/backups・/transfer・/settings・/admin)と/backups/:uuid・/loginが登録済み', () => {
    const paths = routes.map((r) => r.path)
    expect(paths).toContain('/')
    expect(paths).toContain('/login')
    expect(paths).toContain('/backups')
    expect(paths).toContain('/backups/:uuid')
    expect(paths).toContain('/transfer')
    expect(paths).toContain('/settings')
    expect(paths).toContain('/admin')
  })

  it('旧path(/entities・/pending・/diff)は再配置によりroutesへ登録しない', () => {
    const paths = routes.map((r) => r.path)
    expect(paths).not.toContain('/entities')
    expect(paths).not.toContain('/entities/:uuid')
    expect(paths).not.toContain('/pending')
    expect(paths).not.toContain('/diff')
  })
})
