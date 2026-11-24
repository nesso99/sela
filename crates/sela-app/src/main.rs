mod ui;

fn main() {
    if let Some(arg) = std::env::args().nth(1) {
        match arg.as_str() {
            "--version" | "-V" => {
                println!("sela {}", env!("CARGO_PKG_VERSION"));
                return;
            }
            "--help" | "-h" => {
                println!("sela {}", env!("CARGO_PKG_VERSION"));
                println!("Usage: sela [--version | --help]");
                return;
            }
            _ => {}
        }
    }
    ui::run();
}
