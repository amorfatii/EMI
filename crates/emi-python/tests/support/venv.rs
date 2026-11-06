//! Real pytest and coverage.py shared by proving and installed CLI tests.
#![allow(clippy::expect_used)]

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;

pub fn venv() -> &'static Path {
    static VENV: OnceLock<PathBuf> = OnceLock::new();
    VENV.get_or_init(|| {
        let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join("emi-python-venv-2");
        let marker = dir.join(".ready");
        if !marker.exists() {
            // a directory without the marker is a half-made or restored
            // virtualenv (a CI cache keeps target/tmp, whose interpreter
            // links can point at a Python the runner no longer has): `venv`
            // over it repairs nothing, so it is made again from nothing
            let _ = std::fs::remove_dir_all(&dir);
            let created = Command::new("python3")
                .args(["-m", "venv", "--clear"])
                .arg(&dir)
                .output()
                .expect("create virtualenv");
            assert!(created.status.success(), "{created:?}");
            // pip through the virtualenv's interpreter: a `bin/pip` script
            // whose first line names a missing interpreter fails as "not found"
            let installed = Command::new(dir.join("bin/python"))
                .args([
                    "-m",
                    "pip",
                    "install",
                    "--quiet",
                    "pytest>=8,<9",
                    "coverage>=7.9,<8",
                    "hypothesis>=6,<7",
                ])
                .output()
                .expect("install pytest and coverage");
            assert!(installed.status.success(), "{installed:?}");
            std::fs::write(marker, "ok\n").expect("mark virtualenv ready");
        }
        dir
    })
}
