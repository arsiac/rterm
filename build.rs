#[cfg(windows)]
fn main() {
    let mut res = winresource::WindowsResource::new();
    res.set_icon("crates/rterm-gui/icons/app/icon.ico");
    if let Err(e) = res.compile() {
        eprintln!("Error: failed to embed Windows resources: {e}");
        std::process::exit(1);
    }
}

#[cfg(not(windows))]
fn main() {}
