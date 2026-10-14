//! bin entrypoint(設計5章main.rs)。cli::run(argv)の戻り値をexit codeへ渡すのみ。

fn main() {
    std::process::exit(backup_remote::cli::run(std::env::args_os()));
}
