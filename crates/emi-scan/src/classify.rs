//! Path classes for guardrails: which changed files are tests, test or
//! coverage configuration, CI, lockfiles, secret-like, vendored,
//! generated, docs or product code. Classification is by path only, from
//! the conventions of Python, JS/TS, Go, Rust and Java; content-dependent
//! cases (a `[tool.pytest]` section in `pyproject.toml`) are left to the
//! guard, which sees the changed lines.

/// What a path is, for the purpose of judging a change to it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum PathClass {
    Vendored,
    SecretLike,
    Lockfile,
    Ci,
    TestConfig,
    Test,
    Generated,
    Docs,
    Product,
    Other,
}

impl PathClass {
    pub fn as_str(self) -> &'static str {
        match self {
            PathClass::Vendored => "vendored",
            PathClass::SecretLike => "secret_like",
            PathClass::Lockfile => "lockfile",
            PathClass::Ci => "ci",
            PathClass::TestConfig => "test_config",
            PathClass::Test => "test",
            PathClass::Generated => "generated",
            PathClass::Docs => "docs",
            PathClass::Product => "product",
            PathClass::Other => "other",
        }
    }
}

const VENDORED_DIRS: &[&str] = &[
    "vendor",
    "node_modules",
    "third_party",
    "third-party",
    ".venv",
    "site-packages",
];
const LOCKFILES: &[&str] = &[
    "Cargo.lock",
    "package-lock.json",
    "npm-shrinkwrap.json",
    "yarn.lock",
    "pnpm-lock.yaml",
    "bun.lockb",
    "poetry.lock",
    "Pipfile.lock",
    "uv.lock",
    "pdm.lock",
    "go.sum",
    "composer.lock",
    "Gemfile.lock",
];
const CI_FILES: &[&str] = &[
    ".gitlab-ci.yml",
    ".travis.yml",
    "azure-pipelines.yml",
    "Jenkinsfile",
    "bitbucket-pipelines.yml",
    "appveyor.yml",
    ".drone.yml",
];
const CI_DIRS: &[&str] = &[".circleci", ".buildkite", ".woodpecker"];
const SECRET_NAMES: &[&str] = &[
    ".npmrc",
    ".pypirc",
    ".netrc",
    "credentials.json",
    "secrets.json",
    "secrets.yaml",
    "secrets.yml",
];
const SECRET_SUFFIXES: &[&str] = &[".pem", ".key", ".p12", ".pfx", ".keystore", ".jks"];
const TEST_CONFIG_NAMES: &[&str] = &[
    "pytest.ini",
    "conftest.py",
    "tox.ini",
    "noxfile.py",
    ".coveragerc",
    "codecov.yml",
    ".codecov.yml",
    ".nycrc",
    ".nycrc.json",
    ".mocharc.json",
    ".mocharc.yml",
    ".mocharc.js",
    "nextest.toml",
];
const TEST_CONFIG_STEMS: &[&str] = &[
    "jest.config",
    "vitest.config",
    "karma.conf",
    "playwright.config",
    "cypress.config",
];
const EXAMPLE_DIRS: &[&str] = &["example", "examples", "samples", "sample", "demo", "demos"];
const TEST_DIRS: &[&str] = &[
    "test",
    "tests",
    "__tests__",
    "spec",
    "specs",
    "testdata",
    "testing",
];
const DOC_SUFFIXES: &[&str] = &[".md", ".rst", ".adoc", ".txt"];
const SOURCE_SUFFIXES: &[&str] = &[
    ".py", ".pyi", ".js", ".jsx", ".mjs", ".cjs", ".ts", ".tsx", ".mts", ".cts", ".go", ".rs",
    ".java", ".kt", ".rb", ".php", ".c", ".h", ".cc", ".cpp", ".hpp", ".cs", ".swift", ".scala",
];

fn name(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

fn dirs(path: &str) -> impl Iterator<Item = &str> {
    let mut parts: Vec<&str> = path.split('/').collect();
    parts.pop();
    parts.into_iter()
}

fn is_secret(name: &str) -> bool {
    name == ".env"
        || (name.starts_with(".env.") && !name.ends_with(".example") && !name.ends_with(".sample"))
        || name.starts_with("id_rsa")
        || name.starts_with("id_ed25519")
        || SECRET_NAMES.contains(&name)
        || SECRET_SUFFIXES.iter().any(|s| name.ends_with(s))
}

fn is_ci(path: &str, name: &str) -> bool {
    path.starts_with(".github/workflows/")
        || path.starts_with(".github/actions/")
        || CI_FILES.contains(&name)
        || dirs(path).any(|d| CI_DIRS.contains(&d))
}

fn is_test_config(name: &str) -> bool {
    TEST_CONFIG_NAMES.contains(&name)
        || TEST_CONFIG_STEMS
            .iter()
            .any(|s| name.starts_with(&format!("{s}.")))
}

/// Test code by the conventions of each language: test directories,
/// `test_*.py`/`*_test.py`, `*.test.*`/`*.spec.*`, `*_test.go`, Java
/// `src/test/`.
pub fn is_test(path: &str) -> bool {
    let name = name(path);
    if dirs(path).any(|d| TEST_DIRS.contains(&d)) {
        return true;
    }
    if (name.starts_with("test_") && name.ends_with(".py"))
        || name.ends_with("_test.py")
        || name.ends_with("_test.go")
    {
        return true;
    }
    let parts: Vec<&str> = name.split('.').collect();
    parts.len() > 2
        && parts[1..parts.len() - 1]
            .iter()
            .any(|p| matches!(*p, "test" | "spec"))
}

fn is_generated(name: &str) -> bool {
    name.ends_with(".min.js")
        || name.ends_with("_pb2.py")
        || name.ends_with("_pb2_grpc.py")
        || name.ends_with(".pb.go")
        || name.contains(".generated.")
        || name.ends_with("_generated.go")
}

/// Where a test directory and a vendored directory first occur on the
/// path: a test directory first means test fixtures (webpack's
/// `test/configCases/*/node_modules/`), a vendored one first means
/// third-party code that happens to contain tests.
fn first_dirs(path: &str) -> (Option<usize>, Option<usize>) {
    let ds: Vec<&str> = dirs(path).collect();
    (
        ds.iter().position(|d| TEST_DIRS.contains(d)),
        ds.iter().position(|d| VENDORED_DIRS.contains(d)),
    )
}

/// The class of a repository-relative path (forward slashes).
pub fn classify(path: &str) -> PathClass {
    let name = name(path);
    let (test_dir, vendored_dir) = first_dirs(path);
    let fixture = test_dir.is_some_and(|t| vendored_dir.is_none_or(|v| t < v));
    if vendored_dir.is_some() && !fixture {
        PathClass::Vendored
    } else if fixture && !is_test_config(name) {
        PathClass::Test
    } else if !fixture && dirs(path).any(|d| EXAMPLE_DIRS.contains(&d)) {
        PathClass::Docs
    } else if is_secret(name) {
        PathClass::SecretLike
    } else if LOCKFILES.contains(&name) {
        PathClass::Lockfile
    } else if is_ci(path, name) {
        PathClass::Ci
    } else if is_test_config(name) {
        PathClass::TestConfig
    } else if is_test(path) {
        PathClass::Test
    } else if is_generated(name) {
        PathClass::Generated
    } else if path.starts_with("docs/")
        || path.starts_with("doc/")
        || DOC_SUFFIXES.iter().any(|s| name.ends_with(s))
    {
        PathClass::Docs
    } else if SOURCE_SUFFIXES.iter().any(|s| name.ends_with(s)) {
        PathClass::Product
    } else {
        PathClass::Other
    }
}

#[cfg(test)]
mod tests {
    use super::PathClass::*;
    use super::*;

    #[test]
    fn classifies_by_convention() {
        let cases = [
            ("vendor/github.com/x/y.go", Vendored),
            ("web/node_modules/lodash/index.js", Vendored),
            (".env", SecretLike),
            (".env.production", SecretLike),
            (".env.example", Other),
            ("deploy/server.pem", SecretLike),
            ("Cargo.lock", Lockfile),
            ("frontend/package-lock.json", Lockfile),
            ("go.sum", Lockfile),
            (".github/workflows/ci.yml", Ci),
            (".gitlab-ci.yml", Ci),
            ("pytest.ini", TestConfig),
            ("tests/conftest.py", TestConfig),
            ("jest.config.ts", TestConfig),
            (".coveragerc", TestConfig),
            ("tests/test_api.py", Test),
            ("pkg/api_test.py", Test),
            ("src/app.test.ts", Test),
            ("src/__tests__/app.js", Test),
            ("server/handler_test.go", Test),
            ("crates/core/tests/parse.rs", Test),
            ("src/test/java/AppTest.java", Test),
            ("api/v1/service.pb.go", Generated),
            ("dist/app.min.js", Generated),
            ("docs/index.md", Docs),
            ("README.md", Docs),
            ("src/app.py", Product),
            ("crates/core/src/lib.rs", Product),
            ("Makefile", Other),
            ("pyproject.toml", Other),
        ];
        for (path, want) in cases {
            assert_eq!(classify(path), want, "{path}");
        }
    }

    #[test]
    fn fixtures_inside_test_directories_are_tests() {
        assert_eq!(
            classify("test/configCases/perf/node_modules/core-js/index.js"),
            Test
        );
        assert_eq!(
            classify("test/configCases/module/import-meta-env/.env"),
            Test
        );
        assert_eq!(classify("tests/fixtures/project/poetry.lock"), Test);
        assert_eq!(classify("tests/conftest.py"), TestConfig);
        assert_eq!(classify("example/config/.mocharc.js"), Docs);
    }

    #[test]
    fn first_matching_class_wins() {
        // a test inside a vendored tree is vendored, not test
        assert_eq!(classify("vendor/lib/tests/test_x.py"), Vendored);
        // a CI workflow is CI even though it lives under .github
        assert_eq!(classify(".github/workflows/test.yml"), Ci);
    }
}
