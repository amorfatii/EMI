//! The graded profile: function erosion from panel cognitive complexity and
//! Halstead volume, coupling as in `precise`, leftovers unchanged.

use std::path::{Path, PathBuf};

use emi_ir::burden;
use emi_scan::{Options, Probe, Profile, scan};

struct Tree(PathBuf);

impl Drop for Tree {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn tree(name: &str, files: &[(&str, &str)]) -> Tree {
    let root = std::env::temp_dir().join(format!("emi-graded-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    for (rel, text) in files {
        let path = root.join(rel);
        std::fs::create_dir_all(path.parent().unwrap_or(&root)).unwrap_or_default();
        std::fs::write(path, text).unwrap_or_default();
    }
    Tree(root)
}

fn measure(root: &Path, probe: Probe, profile: Profile) -> emi_ir::Measurement {
    let mut options = Options::new(probe);
    options.profile = profile;
    match scan(root, &options) {
        Ok(m) => m,
        Err(e) => panic!("scan failed: {e}"),
    }
}

const PY: &str = "import os\n\nX = 1\n\n\ndef small(a):\n    return a + 1\n\n\ndef branchy(a, b):\n    if a and b:\n        for x in a:\n            if x:\n                return os.sep\n    return None\n";

#[test]
fn python_functions_carry_graded_erosion() {
    let t = tree("py", &[("pkg/m.py", PY)]);
    let graded = measure(&t.0, Probe::Python, Profile::Graded);
    let precise = measure(&t.0, Probe::Python, Profile::Precise);
    let panel = emi_panel::functions(emi_panel::Lang::Python, PY.as_bytes());
    for f in &graded.functions {
        let other = precise
            .functions
            .iter()
            .find(|p| p.line == f.line && p.function == f.function);
        let Some(other) = other else {
            panic!("{} missing under precise", f.function);
        };
        match panel.iter().find(|p| p.start == f.line) {
            Some(p) if !f.function.starts_with("<module") => {
                let want = burden::graded_mass(p.sloc, p.cognitive, p.halstead_volume);
                assert_eq!(f.erosion_mass, Some(want), "{}", f.function);
            }
            // module leftovers keep the adapter's erosion
            _ => assert_eq!(f.erosion_mass, other.erosion_mass, "{}", f.function),
        }
        // coupling is the precise profile's
        assert_eq!(
            f.coupling.as_ref().map(|c| c.resolved_fanout),
            other.coupling.as_ref().map(|c| c.resolved_fanout)
        );
    }
    // the branchy function erodes more than the trivial one
    let mass = |name: &str| {
        graded
            .functions
            .iter()
            .find(|f| f.function == name)
            .and_then(|f| f.erosion_mass)
            .unwrap_or_default()
    };
    assert!(mass("branchy") > mass("small"));
    assert!(graded.measurement_profile.ends_with("+graded-g2"));
    assert_ne!(
        graded.measurement_contract_id,
        precise.measurement_contract_id
    );
}

#[test]
fn native_component_erosion_is_the_sum_of_its_regions() {
    let src = "fn a(x: u32) -> u32 {\n    if x > 1 && x < 9 {\n        return 2;\n    }\n    x\n}\n\nfn b() {}\n";
    let t = tree(
        "rs",
        &[
            ("src/lib.rs", src),
            ("Cargo.toml", "[package]\nname = \"t\"\n"),
        ],
    );
    let m = measure(&t.0, Probe::Rust, Profile::Graded);
    let sum: f64 = m.functions.iter().filter_map(|f| f.erosion_mass).sum();
    let components: f64 = m
        .components
        .iter()
        .map(|c| c.burden.erosion.bounds().0)
        .sum();
    assert!((components - sum).abs() < 1e-9, "{components} vs {sum}");
    assert!(m.measurement_profile.ends_with("+graded-g2"));
}

#[test]
fn profile_names_round_trip_and_unknown_names_are_rejected() {
    for p in [Profile::Parity, Profile::Precise, Profile::Graded] {
        assert_eq!(Profile::from_name(p.name()), Some(p));
    }
    assert_eq!(Profile::from_name("precice"), None);
}
