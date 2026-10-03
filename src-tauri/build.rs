fn main() {
    tauri_build::try_build(tauri_build::Attributes::new().app_manifest(
        tauri_build::AppManifest::new().commands(&[
            "start_scan",
            "cancel_scan",
            "get_last_scan",
            "clean_items",
            "reveal_item",
            "open_trash",
            "get_analysis_locations",
            "get_disk_overview",
            "choose_analysis_directory",
            "analyze_directory",
            "cancel_analysis",
            "reveal_analysis_node",
            "open_privacy_settings",
        ]),
    ))
    .expect("Could not build Mac Sweep");
}
