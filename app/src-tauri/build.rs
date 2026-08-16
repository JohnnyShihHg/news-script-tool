/// Declaring an app ACL manifest is what makes Tauri generate an `allow-<command>`
/// permission for each app-defined command, which is the only way a capability can
/// grant one to a webview loading a remote origin (see `capabilities/collab.json`).
///
/// Note the trade-off: once an app manifest exists, Tauri ACL-gates *every* app
/// command for *every* window -- not just remote ones -- so each command the main
/// window calls must be listed in `capabilities/default.json` too. Anything missing
/// there fails at runtime with "Command X not allowed by ACL", not at compile time.
fn main() {
    tauri_build::try_build(
        tauri_build::Attributes::new().app_manifest(tauri_build::AppManifest::new().commands(&[
            "import_folder",
            "pick_folder",
            "clear_folder",
            "save_text_file",
            "get_config",
            "save_config",
            "reset_config",
            "get_api_key_status",
            "set_api_key",
            "clear_api_key",
            "generate_keywords",
            "generate_keywords_batch",
            "open_collab_window",
            "receive_scraped_text",
            "compare_with_collab_doc",
            "diagnose_collab_bridge",
            "export_collab_text",
            "append_to_collab_doc",
        ])),
    )
    .expect("failed to run tauri-build");
}
