//! Checks the plugin's `.slint` surfaces with the Slint version eov compiles
//! them with:
//!
//! 1. both files compile (as the host compiles them: plugin root as include
//!    path);
//! 2. the components declare exactly the callbacks the plugin handles, with
//!    the argument types it reads;
//! 3. every property the plugin pushes, in a range of realistic states
//!    produced through the SDK's mock host, exists as a public input of the
//!    right type (struct fields included), and every input is pushed;
//! 4. a few controls, found through the accessibility tree, invoke the
//!    callbacks they should;
//! 5. the writing direction eov sets (`eov-rtl`) reaches the surfaces'
//!    `Locale` global, which aligns their texts.
//!
//! Run: `cargo run --release --manifest-path tools/ui-check/Cargo.toml`
//! (exit status 1 on any problem).

use annotations::sidebar;
use i_slint_backend_testing::{AccessibleRole, ElementHandle};
use i_slint_compiler::langtype::Type;
use i_slint_compiler::native_class::PropertyVisibility;
use serde_json::{Map, Value as Json, json};
use slint_interpreter::{ComponentDefinition, ComponentInstance, Value};
use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

mod common;
mod scenario;
use std::process::ExitCode;
use std::rc::Rc;

#[derive(Default)]
struct Report {
    problems: Vec<String>,
}

impl Report {
    fn fail(&mut self, problem: impl Into<String>) {
        let problem = problem.into();
        eprintln!("error: {problem}");
        self.problems.push(problem);
    }
}

fn compile(
    root: &Path,
    file: &str,
    component: &str,
    report: &mut Report,
) -> Option<ComponentDefinition> {
    match common::compile(root, file, component) {
        Ok(definition) => Some(definition),
        Err(errors) => {
            for error in errors {
                report.fail(error);
            }
            None
        }
    }
}

struct Contract {
    inputs: BTreeMap<String, Type>,
    callbacks: BTreeMap<String, Vec<Type>>,
}

fn contract(definition: &ComponentDefinition) -> Contract {
    let mut inputs = BTreeMap::new();
    let mut callbacks = BTreeMap::new();
    for (name, (ty, visibility)) in definition.properties_and_callbacks() {
        match &ty {
            Type::Callback(function) => {
                callbacks.insert(name, function.args.clone());
            }
            ty if ty.is_property_type() => {
                if matches!(
                    visibility,
                    PropertyVisibility::Input | PropertyVisibility::InOut
                ) {
                    inputs.insert(name, ty.clone());
                }
            }
            _ => {}
        }
    }
    Contract { inputs, callbacks }
}

fn kind_of(ty: &Type) -> char {
    match ty {
        Type::String => 's',
        Type::Bool => 'b',
        Type::Int32 | Type::Float32 | Type::Percent => 'n',
        _ => '?',
    }
}

fn check_callbacks(
    file: &str,
    contract: &Contract,
    expected: &[(&str, &str)],
    report: &mut Report,
) {
    let expected: BTreeMap<&str, &str> = expected.iter().copied().collect();
    for (name, args) in &contract.callbacks {
        let declared: String = args.iter().map(kind_of).collect();
        match expected.get(name.as_str()) {
            None => report.fail(format!(
                "{file}: callback '{name}' is not handled by the plugin"
            )),
            Some(sig) if *sig != declared => report.fail(format!(
                "{file}: callback '{name}' takes ({declared}), the plugin reads ({sig})"
            )),
            Some(_) => {}
        }
    }
    for name in expected.keys() {
        if !contract.callbacks.contains_key(*name) {
            report.fail(format!(
                "{file}: the plugin handles '{name}' but no callback declares it"
            ));
        }
    }
}

/// Whether `json` has the shape of `ty`, field by field.
fn shape_error(ty: &Type, json: &Json) -> Option<String> {
    match (ty, json) {
        (Type::String, Json::String(_)) => None,
        (Type::Bool, Json::Bool(_)) => None,
        (Type::Int32, Json::Number(n)) => {
            (n.as_i64().is_none() && n.as_u64().is_none()).then(|| format!("{n} is not an integer"))
        }
        (Type::Float32 | Type::Percent, Json::Number(_)) => None,
        (Type::Color | Type::Brush, Json::String(s)) => {
            slint_interpreter::json::value_from_json(ty, json)
                .err()
                .map(|e| format!("'{s}': {e}"))
        }
        (Type::Array(inner), Json::Array(items)) => items
            .iter()
            .enumerate()
            .find_map(|(i, item)| shape_error(inner, item).map(|e| format!("[{i}]: {e}"))),
        (Type::Struct(s), Json::Object(object)) => {
            let declared: BTreeSet<String> = s.fields.keys().map(|k| k.to_string()).collect();
            let given: BTreeSet<String> = object.keys().map(|k| k.replace('_', "-")).collect();
            if let Some(extra) = given.difference(&declared).next() {
                return Some(format!("field '{extra}' is not declared in the struct"));
            }
            if let Some(missing) = declared.difference(&given).next() {
                return Some(format!("struct field '{missing}' is never pushed"));
            }
            object.iter().find_map(|(k, v)| {
                let field = s.fields.get(k.replace('_', "-").as_str())?;
                shape_error(field, v).map(|e| format!(".{k}: {e}"))
            })
        }
        (ty, json) => Some(format!("{json} does not fit {ty}")),
    }
}

/// Applies properties the way the host does; returns the instance.
fn apply(
    file: &str,
    label: &str,
    definition: &ComponentDefinition,
    contract: &Contract,
    properties: &Map<String, Json>,
    report: &mut Report,
) -> Option<ComponentInstance> {
    let instance = match definition.create() {
        Ok(instance) => instance,
        Err(e) => {
            report.fail(format!("{file}: cannot instantiate: {e}"));
            return None;
        }
    };
    for (name, json) in properties {
        let Some(ty) = contract.inputs.get(name) else {
            report.fail(format!(
                "{file} ({label}): the plugin pushes '{name}', which is not a public input"
            ));
            continue;
        };
        if let Some(problem) = shape_error(ty, json) {
            report.fail(format!("{file} ({label}): property '{name}': {problem}"));
            continue;
        }
        match slint_interpreter::json::value_from_json(ty, json) {
            Ok(value) => {
                if let Err(e) = instance.set_property(name, value) {
                    report.fail(format!("{file} ({label}): cannot set '{name}': {e}"));
                }
            }
            Err(e) => report.fail(format!("{file} ({label}): '{name}': {e}")),
        }
    }
    Some(instance)
}

// ---------------------------------------------------------------------------
// Interactions.
// ---------------------------------------------------------------------------

type Calls = Rc<RefCell<Vec<(String, Vec<Json>)>>>;

fn record_callbacks(instance: &ComponentInstance, definition: &ComponentDefinition) -> Calls {
    let calls: Calls = Rc::default();
    for name in definition.callbacks() {
        let calls = Rc::clone(&calls);
        let callback = name.clone();
        let _ = instance.set_callback(&name, move |args: &[Value]| {
            let args = args
                .iter()
                .map(|v| slint_interpreter::json::value_to_json(v).unwrap_or(Json::Null))
                .collect();
            calls.borrow_mut().push((callback.clone(), args));
            Value::Void
        });
    }
    calls
}

fn press(instance: &ComponentInstance, label: &str) -> bool {
    match ElementHandle::find_by_accessible_label(instance, label).next() {
        Some(element) => {
            element.invoke_accessible_default_action();
            true
        }
        None => false,
    }
}

fn expect_call(report: &mut Report, calls: &Calls, what: &str, name: &str, args: &[Json]) {
    let found = calls
        .borrow()
        .iter()
        .any(|(n, a)| n == name && a.as_slice() == args);
    if !found {
        report.fail(format!(
            "{what}: expected {name}({}) but saw {:?}",
            args.iter()
                .map(Json::to_string)
                .collect::<Vec<_>>()
                .join(", "),
            calls.borrow()
        ));
    }
    calls.borrow_mut().clear();
}

fn interactions(
    definition: &ComponentDefinition,
    contract: &Contract,
    properties: &Map<String, Json>,
    report: &mut Report,
) {
    let Some(instance) = apply(
        sidebar::SIDEBAR_UI,
        "interactions",
        definition,
        contract,
        properties,
        report,
    ) else {
        return;
    };
    let calls = record_callbacks(&instance, definition);
    let step = |label: &str, name: &str, args: Vec<Json>, report: &mut Report| {
        if !press(&instance, label) {
            report.fail(format!("no control labelled '{label}'"));
            return;
        }
        expect_call(report, &calls, &format!("pressing '{label}'"), name, &args);
    };
    let _ = instance.set_property("tab", Value::Number(0.0));
    step("Count", "tab-changed", vec![json!(2)], report);
    step(
        "Count points in hidden layers",
        "count-hidden-toggled",
        vec![json!(true)],
        report,
    );
    step("Data", "tab-changed", vec![json!(3)], report);
    step("Import…", "menu", vec![json!("import")], report);
    step("Back up…", "menu", vec![json!("backup")], report);
    step("Classes", "tab-changed", vec![json!(1)], report);
    step("New class", "class-new", vec![], report);
    step("Layers", "tab-changed", vec![json!(0)], report);
    step("New layer", "layer-new", vec![], report);
    step("Zoom to the annotation", "detail-frame", vec![], report);
    // A text field is one text input for assistive technology, and text set
    // through it is committed like typed text.
    let fields: Vec<ElementHandle> =
        ElementHandle::find_by_accessible_label(&instance, "Annotation name").collect();
    match fields.as_slice() {
        [field] if field.accessible_role() == Some(AccessibleRole::TextInput) => {
            field.set_accessible_value("Renamed");
            expect_call(
                report,
                &calls,
                "setting 'Annotation name'",
                "detail-name-committed",
                &[properties["detail-id"].clone(), json!("Renamed")],
            );
        }
        _ => report.fail(format!(
            "expected one text input labelled 'Annotation name', found {:?}",
            fields
                .iter()
                .map(ElementHandle::accessible_role)
                .collect::<Vec<_>>()
        )),
    }
    // A list row: click selects it.
    if let Some(row) = properties["rows"]
        .as_array()
        .and_then(|rows| rows.iter().find(|r| r["kind"] == 1))
    {
        let label = row["label"].as_str().unwrap_or_default().to_string();
        step(
            &label,
            "row-clicked",
            vec![row["id"].clone(), row["layer-id"].clone(), json!(false)],
            report,
        );
    }
}

fn import_interactions(
    definition: &ComponentDefinition,
    contract: &Contract,
    properties: &Map<String, Json>,
    report: &mut Report,
) {
    let Some(instance) = apply(
        sidebar::IMPORT_UI,
        "interactions",
        definition,
        contract,
        properties,
        report,
    ) else {
        return;
    };
    let calls = record_callbacks(&instance, definition);
    for (label, name, args) in [
        ("Cancel", "import-cancel", vec![]),
        ("Import", "import-confirm", vec![json!(0)]),
    ] {
        if !press(&instance, label) {
            report.fail(format!("import dialog: no control labelled '{label}'"));
            continue;
        }
        expect_call(
            report,
            &calls,
            &format!("import dialog '{label}'"),
            name,
            &args,
        );
    }
}

/// Inputs eov sets on every surface that declares them (the language of the
/// interface); the plugin does not push them.
const HOST_INPUTS: &[&str] = &["eov-rtl", "eov-language"];

/// Setting `eov-rtl`, as eov does for Arabic, Persian and Hebrew, makes the
/// surface's `Locale` global right-to-left.
fn check_direction(file: &str, definition: &ComponentDefinition, report: &mut Report) {
    let instance = match definition.create() {
        Ok(instance) => instance,
        Err(e) => {
            report.fail(format!("{file}: cannot instantiate: {e}"));
            return;
        }
    };
    for rtl in [true, false] {
        if let Err(e) = instance.set_property("eov-rtl", Value::Bool(rtl)) {
            report.fail(format!("{file}: cannot set 'eov-rtl': {e}"));
            return;
        }
        match instance.get_global_property("Locale", "rtl") {
            Ok(Value::Bool(value)) if value == rtl => {}
            other => report.fail(format!(
                "{file}: eov-rtl = {rtl}, but Locale.rtl is {other:?}"
            )),
        }
    }
}

fn check_surface(
    file: &str,
    definition: &ComponentDefinition,
    contract: &Contract,
    samples: &[(String, Map<String, Json>)],
    report: &mut Report,
) {
    let mut pushed = BTreeSet::new();
    for (label, properties) in samples {
        pushed.extend(properties.keys().cloned());
        apply(file, label, definition, contract, properties, report);
    }
    for name in contract.inputs.keys() {
        if HOST_INPUTS.contains(&name.as_str()) {
            let expected = if name == "eov-rtl" {
                Type::Bool
            } else {
                Type::String
            };
            if contract.inputs.get(name) != Some(&expected) {
                report.fail(format!("{file}: eov sets '{name}' as {expected}"));
            }
            continue;
        }
        if !pushed.contains(name) {
            report.fail(format!(
                "{file}: input property '{name}' is never pushed by the plugin"
            ));
        }
    }
    println!(
        "{file}: {} inputs, {} callbacks, {} states checked",
        contract.inputs.len(),
        contract.callbacks.len(),
        samples.len()
    );
}

fn main() -> ExitCode {
    i_slint_backend_testing::init_no_event_loop();
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    let root = root.canonicalize().unwrap_or(root);
    let mut report = Report::default();

    let sidebar_def = compile(
        &root,
        sidebar::SIDEBAR_UI,
        sidebar::SIDEBAR_COMPONENT,
        &mut report,
    );
    let import_def = compile(
        &root,
        sidebar::IMPORT_UI,
        sidebar::IMPORT_COMPONENT,
        &mut report,
    );
    let samples = match scenario::samples() {
        Ok(samples) => Some(samples),
        Err(e) => {
            report.fail(format!("scenario: {e}"));
            None
        }
    };
    if let Some(definition) = &sidebar_def {
        let contract = contract(definition);
        check_callbacks(
            sidebar::SIDEBAR_UI,
            &contract,
            sidebar::CALLBACKS,
            &mut report,
        );
        if let Some(samples) = &samples {
            check_surface(
                sidebar::SIDEBAR_UI,
                definition,
                &contract,
                &samples.sidebar,
                &mut report,
            );
            check_direction(sidebar::SIDEBAR_UI, definition, &mut report);
            if let Some((_, properties)) = samples
                .sidebar
                .iter()
                .find(|(label, _)| label == "one selected")
            {
                interactions(definition, &contract, properties, &mut report);
            }
        }
    }
    if let Some(definition) = &import_def {
        let contract = contract(definition);
        check_callbacks(
            sidebar::IMPORT_UI,
            &contract,
            sidebar::IMPORT_CALLBACKS,
            &mut report,
        );
        if let Some(samples) = &samples {
            check_surface(
                sidebar::IMPORT_UI,
                definition,
                &contract,
                &samples.import,
                &mut report,
            );
            check_direction(sidebar::IMPORT_UI, definition, &mut report);
            if let Some((_, properties)) = samples.import.first() {
                import_interactions(definition, &contract, properties, &mut report);
            }
        }
    }
    if report.problems.is_empty() {
        println!("ui-check: OK");
        ExitCode::SUCCESS
    } else {
        eprintln!("ui-check: {} problem(s)", report.problems.len());
        ExitCode::FAILURE
    }
}
