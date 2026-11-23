//! Compiling surfaces the way the host does (`app/src/host_ui/surfaces.rs`).

use slint_interpreter::ComponentDefinition;
use std::path::Path;

fn spin_on<T>(future: impl std::future::Future<Output = T>) -> T {
    use std::task::{Context, Poll, Waker};
    let mut cx = Context::from_waker(Waker::noop());
    let mut future = std::pin::pin!(future);
    loop {
        if let Poll::Ready(value) = future.as_mut().poll(&mut cx) {
            return value;
        }
    }
}

/// Compiles `file` (relative to the plugin root, which is also the include
/// path) and returns `component`; warnings are printed. Notes (such as that
/// no edition is set: the host sets none) are skipped, as Slint's build does.
pub fn compile(
    root: &Path,
    file: &str,
    component: &str,
) -> Result<ComponentDefinition, Vec<String>> {
    let mut compiler = slint_interpreter::Compiler::default();
    compiler.set_include_paths(vec![root.to_path_buf()]);
    let result = spin_on(compiler.build_from_path(root.join(file)));
    let mut errors = Vec::new();
    for d in result.diagnostics() {
        match d.level() {
            slint_interpreter::DiagnosticLevel::Error => errors.push(format!("{file}: {d}")),
            slint_interpreter::DiagnosticLevel::Info => {}
            _ => eprintln!("warning: {file}: {d}"),
        }
    }
    if !errors.is_empty() {
        return Err(errors);
    }
    result
        .component(component)
        .ok_or_else(|| vec![format!("{file} does not export {component}")])
}
