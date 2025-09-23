#[cfg(all(target_os = "windows", target_arch = "x86_64"))]
fn main() {
    println!(r"cargo:rustc-link-search=vendor/mpv-dev-x86_64");
}

#[cfg(all(target_os = "windows", target_arch = "aarch64"))]
fn main() {
    println!(r"cargo:rustc-link-search=vendor/mpv-dev-aarch64");
}

#[cfg(target_os = "macos")]
fn main() {
    println!(r"cargo:rustc-link-search=/opt/homebrew/Cellar/mpv/*/lib");
}

#[cfg(not(any(target_os = "windows", target_os = "macos")))]
fn main() {}
