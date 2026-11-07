fn main() {
    // Only the desktop app is a Tauri app; the web server (`--no-default-features --features web`) needs no Tauri build step.
    #[cfg(feature = "desktop")]
    tauri_build::build()
}
