//! Draws the plugin's surfaces, in the states `scenario` produces, into PNG
//! files with Slint's software renderer, to look at the layout without
//! running eov.
//!
//! Run: `cargo run --release --manifest-path tools/ui-check/Cargo.toml --bin ui-render -- <out-dir>`

mod common;
mod scenario;

use annotations::sidebar;
use slint::platform::software_renderer::{
    MinimalSoftwareWindow, PremultipliedRgbaColor, RepaintBufferType,
};
use slint::platform::{Platform, WindowAdapter, WindowEvent};
use slint::{ComponentHandle, PhysicalSize};
use slint_interpreter::{ComponentDefinition, ComponentInstance, Value};
use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::rc::Rc;
use std::time::{Duration, Instant};

thread_local! {
    static WINDOWS: RefCell<Vec<Rc<MinimalSoftwareWindow>>> = const { RefCell::new(Vec::new()) };
}

struct RenderPlatform {
    start: Instant,
}

impl Platform for RenderPlatform {
    fn create_window_adapter(&self) -> Result<Rc<dyn WindowAdapter>, slint::PlatformError> {
        let window = MinimalSoftwareWindow::new(RepaintBufferType::NewBuffer);
        WINDOWS.with(|w| w.borrow_mut().push(window.clone()));
        Ok(window)
    }

    fn duration_since_start(&self) -> Duration {
        self.start.elapsed()
    }
}

const SCALE: f32 = 2.0;

fn instantiate(
    definition: &ComponentDefinition,
    properties: &serde_json::Map<String, serde_json::Value>,
) -> Result<(ComponentInstance, Rc<MinimalSoftwareWindow>), String> {
    let instance = definition.create().map_err(|e| e.to_string())?;
    let window = WINDOWS
        .with(|w| w.borrow().last().cloned())
        .ok_or("no window was created")?;
    let types: std::collections::HashMap<String, _> = definition
        .properties_and_callbacks()
        .map(|(name, (ty, _))| (name, ty))
        .collect();
    for (name, json) in properties {
        let Some(ty) = types.get(name) else {
            continue;
        };
        let value = slint_interpreter::json::value_from_json(ty, json)
            .map_err(|e| format!("{name}: {e}"))?;
        instance
            .set_property(name, value)
            .map_err(|e| format!("{name}: {e}"))?;
    }
    Ok((instance, window))
}

fn render(
    instance: &ComponentInstance,
    window: &Rc<MinimalSoftwareWindow>,
    width: u32,
    height: u32,
    path: &Path,
) -> Result<(), String> {
    let (w, h) = (
        (width as f32 * SCALE) as u32,
        (height as f32 * SCALE) as u32,
    );
    window.dispatch_event(WindowEvent::ScaleFactorChanged {
        scale_factor: SCALE,
    });
    window.set_size(PhysicalSize::new(w, h));
    instance.show().map_err(|e| e.to_string())?;
    for _ in 0..3 {
        slint::platform::update_timers_and_animations();
    }
    let mut buffer = vec![PremultipliedRgbaColor::default(); (w * h) as usize];
    window.request_redraw();
    window.draw_if_needed(|renderer| {
        renderer.render(&mut buffer, w as usize);
    });
    let mut bytes = Vec::with_capacity(buffer.len() * 4);
    for p in &buffer {
        bytes.extend_from_slice(&[p.red, p.green, p.blue, 255]);
    }
    let file = std::fs::File::create(path).map_err(|e| e.to_string())?;
    let mut encoder = png::Encoder::new(std::io::BufWriter::new(file), w, h);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    let mut writer = encoder.write_header().map_err(|e| e.to_string())?;
    writer.write_image_data(&bytes).map_err(|e| e.to_string())?;
    let _ = instance.hide();
    println!("{}", path.display());
    Ok(())
}

fn press(instance: &ComponentInstance, label: &str) {
    if let Some(element) =
        i_slint_backend_testing::ElementHandle::find_by_accessible_label(instance, label).next()
    {
        element.invoke_accessible_default_action();
    } else {
        eprintln!("no control labelled '{label}'");
    }
}

fn run(out: &Path) -> Result<(), String> {
    slint::platform::set_platform(Box::new(RenderPlatform {
        start: Instant::now(),
    }))
    .map_err(|e| format!("{e:?}"))?;
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    let root = root.canonicalize().unwrap_or(root);
    let sidebar_def = common::compile(&root, sidebar::SIDEBAR_UI, sidebar::SIDEBAR_COMPONENT)
        .map_err(|e| e.join("\n"))?;
    let import_def = common::compile(&root, sidebar::IMPORT_UI, sidebar::IMPORT_COMPONENT)
        .map_err(|e| e.join("\n"))?;
    let samples = scenario::samples()?;
    std::fs::create_dir_all(out).map_err(|e| e.to_string())?;
    for (i, (label, properties)) in samples.sidebar.iter().enumerate() {
        let (instance, window) = instantiate(&sidebar_def, properties)?;
        let name = format!("sidebar-{i}-{}.png", label.replace(' ', "-"));
        render(&instance, &window, 340, 860, &out.join(name))?;
    }
    // Every tab, and the popups, in the "one selected" state (the whole
    // Data tab in the "data tab" state, with the other slides listed and a
    // comparison with a second reader).
    let sample = |label: &str| {
        samples
            .sidebar
            .iter()
            .find(|(l, _)| l == label)
            .map(|(_, p)| p)
    };
    if let Some(properties) = sample("one selected") {
        for (tab, name, height, state) in [
            (1, "classes", 860, "one selected"),
            (2, "count", 860, "one selected"),
            (3, "data", 860, "one selected"),
            (3, "data-whole", 2200, "data tab"),
            (0, "layers-whole", 2200, "one selected"),
        ] {
            let properties = sample(state).unwrap_or(properties);
            let (instance, window) = instantiate(&sidebar_def, properties)?;
            instance
                .set_property("tab", Value::Number(tab as f64))
                .map_err(|e| e.to_string())?;
            render(
                &instance,
                &window,
                340,
                height,
                &out.join(format!("tab-{name}.png")),
            )?;
        }
        for (label, file) in [
            ("Annotation class", "popup-class.png"),
            ("Import and export", "popup-menu.png"),
            ("Layer settings", "layer-settings.png"),
            ("Layer colour", "popup-palette.png"),
        ] {
            let (instance, window) = instantiate(&sidebar_def, properties)?;
            instance
                .set_property("tab", Value::Number(0.0))
                .map_err(|e| e.to_string())?;
            // Lay out first: popups open at the control's position.
            render(&instance, &window, 340, 860, &out.join("scratch.png"))?;
            if label == "Layer colour" {
                press(&instance, "Layer settings");
                render(&instance, &window, 340, 860, &out.join("scratch.png"))?;
            }
            press(&instance, label);
            render(&instance, &window, 340, 860, &out.join(file))?;
        }
        let _ = std::fs::remove_file(out.join("scratch.png"));
    }
    for (label, properties) in &samples.import {
        let (instance, window) = instantiate(&import_def, properties)?;
        render(
            &instance,
            &window,
            520,
            400,
            &out.join(format!("{}.png", label.replace(' ', "-"))),
        )?;
    }
    Ok(())
}

/// `ui-render --file <root> <file.slint> <Component> <width> <height> <out.png>`
/// draws any component (for layout experiments).
fn render_file(args: &[String]) -> Result<(), String> {
    let [root, file, component, width, height, out] = args else {
        return Err(
            "usage: --file <root> <file.slint> <Component> <width> <height> <out.png>".into(),
        );
    };
    slint::platform::set_platform(Box::new(RenderPlatform {
        start: Instant::now(),
    }))
    .map_err(|e| format!("{e:?}"))?;
    let definition = common::compile(Path::new(root), file, component).map_err(|e| e.join("\n"))?;
    let (instance, window) = instantiate(&definition, &serde_json::Map::new())?;
    let size = |v: &String| v.parse::<u32>().map_err(|e| e.to_string());
    render(
        &instance,
        &window,
        size(width)?,
        size(height)?,
        Path::new(out),
    )
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.first().map(String::as_str) == Some("--file") {
        return match render_file(&args[1..]) {
            Ok(()) => ExitCode::SUCCESS,
            Err(e) => {
                eprintln!("ui-render: {e}");
                ExitCode::FAILURE
            }
        };
    }
    let out = std::env::args()
        .nth(1)
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("ui-render"));
    match run(&out) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("ui-render: {e}");
            ExitCode::FAILURE
        }
    }
}
