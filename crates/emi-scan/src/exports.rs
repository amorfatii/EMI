//! The names a source file exports, read by its language adapter: the one
//! entry point callers outside the adapters use.

use emi_ir::exports::Export;
use emi_panel::Lang;

/// A file's exported names, or why there are none to compare.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Exports {
    /// No adapter reads this file's language.
    Unsupported,
    /// The adapter could not read the names (a syntax error, or a Python
    /// `__all__` that is not a literal): they are unknown, not empty.
    Unparsed,
    Names(Vec<Export>),
}

/// Exported names of `source`, chosen by the extension of `path` with the
/// panel's extension table.
pub fn exports(path: &str, source: &str) -> Exports {
    let Some(lang) = Lang::of_path(path) else {
        return Exports::Unsupported;
    };
    let names = match lang {
        Lang::Python => emi_lang_python::exports::exports(source),
        Lang::JavaScript | Lang::TypeScript | Lang::Tsx => {
            emi_lang_js::exports::exports(path, source)
        }
        Lang::Go => emi_lang_go::coupling::exports(source),
        Lang::Rust => emi_lang_rust::exports::exports(source),
    };
    names.map_or(Exports::Unparsed, Exports::Names)
}
