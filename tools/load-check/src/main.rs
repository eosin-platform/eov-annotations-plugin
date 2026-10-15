//! Loads the built plugin library the way eov does (abi_stable's layout
//! checks, then the ABI and plugin-API version handshake) and drives the
//! instance through a few events across the FFI boundary, with the SDK's
//! mock host in temporary directories.
//!
//! Run: `cargo run --release --manifest-path tools/load-check/Cargo.toml -- [library]`
//! (default: `target/release/<platform library name>` of the plugin).

use abi_stable::std_types::RStr;
use eov_plugin_api::abi::{self, PluginBox, PluginModuleRef};
use eov_plugin_api::prelude::*;
use eov_plugin_api::testing::{MockHost, test_init};
use serde_json::Value as Json;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

fn send(plugin: &PluginBox, event: &HostEvent) -> Result<Json, String> {
    let json = serde_json::to_string(event).map_err(|e| e.to_string())?;
    let reply = plugin
        .handle_event(RStr::from_str(&json))
        .into_result()
        .map_err(|e| e.message.into_string())?;
    serde_json::from_str(reply.as_str()).map_err(|e| e.to_string())
}

fn settle(plugin: &PluginBox, host: &MockHost) -> Result<(), String> {
    for _ in 0..50 {
        send(
            plugin,
            &HostEvent::UiCallback {
                surface_id: String::new(),
                name: "__sync".into(),
                args: vec![],
            },
        )?;
        let posted: Vec<String> = std::mem::take(&mut host.state().posted);
        if posted.is_empty() {
            return Ok(());
        }
        for token in posted {
            send(plugin, &HostEvent::Posted { token })?;
        }
    }
    Ok(())
}

fn run(path: &Path) -> Result<(), String> {
    let header = abi_stable::library::lib_header_from_path(path)
        .map_err(|e| format!("{}: {e}", path.display()))?;
    let module = header
        .init_root_module::<PluginModuleRef>()
        .map_err(|e| format!("not loadable by this plugin API: {e}"))?;
    if module.abi_version() != abi::ABI_VERSION {
        return Err(format!(
            "ABI {} (expected {})",
            module.abi_version(),
            abi::ABI_VERSION
        ));
    }
    let api = module.api_version().as_str();
    let major = |v: &str| v.split('.').next().unwrap_or_default().to_string();
    if major(api) != major(abi::API_VERSION) {
        return Err(format!(
            "built for plugin API {api}, expected {}",
            abi::API_VERSION
        ));
    }
    println!(
        "{}: ABI {}, plugin API {api}",
        path.display(),
        module.abi_version()
    );

    let host = MockHost::new();
    let init = test_init("annotations");
    let dir = PathBuf::from(&init.cache_dir);
    let init_json = serde_json::to_string(&init).map_err(|e| e.to_string())?;
    let plugin = (module.create())(host.to_ffi(), RStr::from_str(&init_json))
        .into_result()
        .map_err(|e| format!("create failed: {}", e.message.into_string()))?;
    settle(&plugin, &host)?;
    {
        let state = host.state();
        for tool in ["select", "polygon", "arrow", "count", "field"] {
            if !state.tools.contains_key(tool) {
                return Err(format!("the '{tool}' tool was not registered"));
            }
        }
    }

    let slide = dir.join("slide.svs");
    std::fs::write(&slide, vec![7u8; 100_000]).map_err(|e| e.to_string())?;
    let file = FileInfo {
        file_id: 1,
        path: slide.to_string_lossy().into_owned(),
        filename: "slide.svs".into(),
        width: 10_000,
        height: 8_000,
        mpp_x: Some(0.25),
        mpp_y: Some(0.25),
        identity: Some("eov-slide-1:load-check".into()),
        ..Default::default()
    };
    host.state().snapshot.files.push(file.clone());
    send(&plugin, &HostEvent::FileOpened { file })?;
    send(
        &plugin,
        &HostEvent::ActiveChanged {
            pane: Some(1),
            file_id: Some(1),
        },
    )?;
    settle(&plugin, &host)?;
    send(
        &plugin,
        &HostEvent::ShapeDrawn {
            tool_id: "polygon".into(),
            pane: 1,
            file_id: 1,
            shape: Shape::Polygon {
                exterior: vec![
                    [100.0, 100.0],
                    [900.0, 100.0],
                    [900.0, 700.0],
                    [100.0, 700.0],
                ],
                holes: vec![],
            },
            modifiers: Modifiers::default(),
        },
    )?;
    settle(&plugin, &host)?;
    let objects: usize = host
        .state()
        .layers
        .iter()
        .filter(|(id, _)| id.starts_with("ann.1."))
        .map(|(_, layer)| layer.objects.len())
        .sum();
    let undo = host.state().undo.len();
    if objects != 1 || undo != 1 {
        return Err(format!(
            "after drawing: {objects} object(s), {undo} undo entries (expected 1 and 1)"
        ));
    }
    let db = PathBuf::from(&init.data_dir).join("annotations.db");
    if !db.is_file() {
        return Err(format!("no database at {}", db.display()));
    }
    plugin.shutdown();
    println!("load-check: OK (tools registered, a polygon drawn, stored and undoable)");
    Ok(())
}

fn main() -> ExitCode {
    let path = std::env::args()
        .nth(1)
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("../../target/release")
                .join(abi::plugin_library_filename("annotations"))
        });
    match run(&path) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("load-check: {e}");
            ExitCode::FAILURE
        }
    }
}
