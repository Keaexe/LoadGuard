fn main() {
    if let Err(e) = slint_build::compile("ui/main.slint") {
        eprintln!("{e}");
        std::process::exit(1);
    }
}
