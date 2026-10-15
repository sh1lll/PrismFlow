#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let store = prismflow_lib::credentials::default_store();
    let mut stdout = std::io::stdout();
    if let Some(code) = prismflow_lib::cli::handle_args(&args, store.as_ref(), &mut stdout) {
        std::process::exit(code);
    }
    prismflow_lib::run()
}
