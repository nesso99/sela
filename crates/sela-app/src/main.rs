mod ui;

fn version() -> String {
    match option_env!("SELA_GIT_SHA") {
        Some(sha) => format!("sela {} ({sha})", env!("CARGO_PKG_VERSION")),
        None => format!("sela {}", env!("CARGO_PKG_VERSION")),
    }
}

fn main() {
    if let Some(arg) = std::env::args().nth(1) {
        match arg.as_str() {
            "--version" | "-V" => {
                println!("{}", version());
                return;
            }
            "--help" | "-h" => {
                println!("{}", version());
                println!("Usage: sela [--version | --help]");
                return;
            }
            _ => {}
        }
    }
    ui::run();
}
