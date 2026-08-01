fn main() {
    // PROCESS_PER_MONITOR_DPI_AWARE_V2 (ADR-010) via an explicit manifest,
    // see resources/resticker.exe.manifest.
    let attrs = tauri_build::Attributes::new().windows_attributes(
        tauri_build::WindowsAttributes::new()
            .app_manifest(include_str!("resources/resticker.exe.manifest")),
    );
    tauri_build::try_build(attrs).expect("tauri_build failed");
}
