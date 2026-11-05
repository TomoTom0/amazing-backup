import { describe, expect, it } from 'vitest'
import { routes } from './routes'

// routerのentities画面登録(TASK-9設計7章・コードレビュー指摘高4)。routes配列は
// router/index.tsから切り出したfrontend/src/router/routes.tsを参照する
// (=createWebHistoryがnode環境で動かないための観測seam。index.tsはroutesを
// importして組立てる)。実装前のため./routesが未作成=本fileのtestは実行failを
// 許容(実装後に緑化)。HomeViewの「後続phase」文言解消後のnav link実描画・
// router guardの実navigationはcomponent実描画と同様に実browser観測のため
// 条件書frontend.tomlのexcluded参照。

// [covers:frontend.entities-routes-nav]
describe('routerのentities画面登録(routes)', () => {
  it('/entities(EntitiesView)と/entities/:uuid(EntityDetailView)が登録済み', () => {
    const paths = routes.map((r) => r.path)
    expect(paths).toContain('/entities')
    expect(paths).toContain('/entities/:uuid')
  })
})
