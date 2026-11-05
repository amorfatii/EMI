//! The pins plugin (`emi_pins.py`) and where a run finds it.

use std::path::Path;

use emi_kernel::pins::{PLUGIN, PLUGIN_DIR};

/// The plugin's source.
pub const SOURCE: &str = include_str!("emi_pins.py");

/// Write the plugin into `workspace`, where a run's `PYTHONPATH` names it
/// ([`emi_kernel::pins::Pins::python_path`]). The directory is EMI's own: a
/// candidate that ships one is overwritten, never trusted.
pub fn install(workspace: &Path) -> Result<(), String> {
    let dir = workspace.join(PLUGIN_DIR);
    if dir.symlink_metadata().is_ok() {
        std::fs::remove_dir_all(&dir)
            .or_else(|_| std::fs::remove_file(&dir))
            .map_err(|e| format!("cannot replace {}: {e}", dir.display()))?;
    }
    std::fs::create_dir(&dir).map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
    std::fs::write(dir.join(format!("{PLUGIN}.py")), SOURCE)
        .map_err(|e| format!("cannot write the pins plugin: {e}"))
}
