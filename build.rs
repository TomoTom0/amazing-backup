//! TASK-9設計10章(dist供給の二階建て): frontend/distの有無をbuild時に通知する。
//! dist欠落はerrorにしない(rust-embedの#[allow_missing]によりbinaryはbuild可能。
//! serveはdist欠落時も起動しAPIのみで動作+warning表示)。cargo check/rust-analyzerから
//! pnpmは呼ばない(build手順の表示のみ)。

fn main() {
    println!("cargo:rerun-if-changed=frontend/dist");
    let dist = std::path::Path::new("frontend/dist");
    let empty = std::fs::read_dir(dist)
        .map(|mut it| it.next().is_none())
        .unwrap_or(true);
    if empty {
        println!(
            "cargo:warning=frontend/distが無いためfrontendを埋め込まずにbuildします(serveはAPIのみで起動)。frontend build手順: cd frontend && pnpm install && pnpm build"
        );
    }
}
