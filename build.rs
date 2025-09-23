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
    use std::fs;
    let entry: fs::DirEntry = fs::read_dir("/opt/homebrew/Cellar/mpv/")
        .expect("mpv not installed (read_dir)")
        .next()
        .expect("mpv not installed (no subdir)")
        .expect("mpv not installed (cannot read subdir)");
    let mut lib_dir = entry.path().to_owned();
    lib_dir.push("lib");

    println!("cargo:rustc-link-search={}", lib_dir.to_string_lossy());
}

#[cfg(not(any(target_os = "windows", target_os = "macos")))]
fn main() {}
