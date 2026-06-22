fn main() {
    // macOS: add rpath for Swift runtime dylibs (needed by macos-translate).
    println!("cargo:rustc-link-arg=-Wl,-rpath,/usr/lib/swift");
}
