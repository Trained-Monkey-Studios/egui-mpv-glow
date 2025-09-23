#[cfg(target_arch = "x86_64")]
fn main() {
    println!(r"cargo:rustc-link-search=vendor/mpv-dev-x86_64");
}

#[cfg(target_arch = "aarch64")]
fn main() {
    println!(r"cargo:rustc-link-search=vendor/mpv-dev-aarch64");
}
