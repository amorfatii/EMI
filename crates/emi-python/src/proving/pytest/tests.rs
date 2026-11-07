use super::*;

fn argv(words: &str) -> Vec<String> {
    words.split_whitespace().map(str::to_owned).collect()
}

fn command(words: &str) -> Command {
    Command::parse(&argv(words)).unwrap_or_else(|e| panic!("{words}: {e}"))
}

#[test]
fn only_pytest_invocations_are_recognised() {
    for ok in [
        "python -m pytest -q",
        "python3 -m pytest",
        ".venv/bin/python -m pytest tests/",
        "pytest -q",
        ".venv/bin/pytest -x",
        "py.test",
    ] {
        assert!(Command::parse(&argv(ok)).is_ok(), "{ok}");
    }
    for refused in [
        "make test",
        "tox -e py",
        "python -m unittest",
        "python script.py",
        "python -m pytest_wrapper",
        "npm test",
        "uv run pytest",
    ] {
        let why = Command::parse(&argv(refused)).err();
        assert!(
            why.is_some_and(|w| w.contains("not a pytest invocation")),
            "{refused}"
        );
    }
    assert!(Command::parse(&[]).is_err());
}

#[test]
fn listing_keeps_the_selectors_and_one_quiet_flag() {
    let c = command("python -m pytest -q -vv -k slow tests/unit -x");
    let listing = c.list_argv();
    // the tests' own Python runs the reporter, which calls pytest
    assert_eq!((listing[0].as_str(), listing[1].as_str()), ("python", "-c"));
    assert!(listing[2].contains("EMI-ITEM"));
    assert_eq!(
        listing[3..],
        argv(
            "-k slow -x tests/unit --collect-only -q -p emi_pins -p no:randomly -p no:cacheprovider"
        )
    );
}

#[test]
fn the_reporters_listing_is_exact_and_must_be_whole() {
    let out = "<Module tests/t.py>\n  <Function test_a>\n\nEMI-ITEM tests/t.py::test_a\nEMI-ITEM tests/t.py::test_p[x y]\nEMI-BROKEN tests/broken.py\nEMI-ITEMS 2\n";
    let ids = parse_listing(out).unwrap_or_default();
    assert_eq!(ids.len(), 2);
    assert!(ids.contains("tests/t.py::test_p[x y]"));
    assert_eq!(
        uncollected_files(out).into_iter().collect::<Vec<_>>(),
        ["tests/broken.py"]
    );
    assert!(parse_listing("EMI-ITEM tests/t.py::test_a\n").is_err());
    assert!(parse_listing("EMI-ITEM tests/t.py::test_a\nEMI-ITEMS 3\n").is_err());
}

#[test]
fn running_chosen_tests_drops_the_selectors_and_what_hides_failures() {
    // the project's traceback style gives way to Python's own
    let c = command("python -m pytest -q -x --maxfail 2 --tb=long -k fast tests/ tests/a.py::t");
    let ids = argv("tests/test_a.py::test_new tests/test_a.py::test_p[1]");
    assert_eq!(
        c.run_argv(&ids, ".emi-proving/j.xml"),
        argv(
            "python -m pytest -q -k fast --continue-on-collection-errors --tb=native -p emi_pins \
             -p no:randomly -p no:cacheprovider --junitxml=.emi-proving/j.xml tests/test_a.py::test_new tests/test_a.py::test_p[1]"
        )
    );
    // the value of an option stays with it, and `--maxfail=3` goes whole
    let c = command("pytest --maxfail=3 --ignore tests/slow -p xdist");
    assert_eq!(
        c.run_argv(&[], "j.xml"),
        argv(
            "pytest --ignore tests/slow -p xdist --continue-on-collection-errors --tb=native \
             -p emi_pins -p no:randomly -p no:cacheprovider --junitxml=j.xml"
        )
    );
}

#[test]
fn coverage_runs_pytest_under_the_same_python() {
    let c = command(".venv/bin/pytest -q");
    let steps = c.coverage_argv(
        &argv("tests/test_a.py::t"),
        ".emi-proving/coverage.ini",
        ".emi-proving/coverage.json",
    );
    assert_eq!(steps.len(), 1, "the run and the report in one interpreter");
    let step = &steps[0];
    assert_eq!(
        (step[0].as_str(), step[1].as_str()),
        (".venv/bin/python", "-c")
    );
    assert!(step[2].contains("json_report"), "{}", step[2]);
    assert_eq!(
        step[3..7],
        argv(".emi-proving/coverage.ini .emi-proving/coverage.json -p emi_proving_contexts")[..]
    );
    assert_eq!(step.last().map(String::as_str), Some("tests/test_a.py::t"));
    // a bare `pytest` has no interpreter beside it
    let bare = command("pytest").coverage_argv(&[], "settings.ini", "c.json");
    assert_eq!(bare[0][0], "python3");
    assert!(!bare[0].iter().any(|a| a.starts_with("--source")));
}

#[test]
fn the_listing_is_the_node_ids_before_the_summary() {
    let out = "tests/test_a.py::TestK::test_m\ntests/test_a.py::test_p[x y]\ntests/test_a.py::test_p[z.w::q]\n\n2 warnings\ntests/test_x.py:3: Warning: a::b\n8 tests collected in 0.01s\n";
    let got = parse_listing(out).unwrap_or_default();
    assert_eq!(
        got.into_iter().collect::<Vec<_>>(),
        vec![
            "tests/test_a.py::TestK::test_m",
            "tests/test_a.py::test_p[x y]",
            "tests/test_a.py::test_p[z.w::q]",
        ]
    );
}

#[test]
fn a_listing_of_file_counts_is_an_error_not_an_empty_list() {
    let out = "tests/test_a.py: 3\ntests/test_c.py: 1\n\n4 tests collected in 0.01s\n";
    assert!(parse_listing(out).is_err());
    // a listing that is simply empty is empty
    assert_eq!(parse_listing("\n").map(|s| s.len()), Ok(0));
}

fn set(ids: &[&str]) -> BTreeSet<String> {
    ids.iter().map(|s| (*s).to_owned()).collect()
}

#[test]
fn added_tests_are_the_ids_only_the_candidate_lists() {
    let baseline = set(&["t.py::a", "t.py::b", "t.py::c[1]", "gone.py::x"]);
    let candidate = set(&[
        "t.py::a",
        // an old test whose body changed keeps its id: not added
        "t.py::b",
        // a new parameter of an old test is a new id
        "t.py::c[1]",
        "t.py::c[2]",
        "t.py::new",
        "new_file.py::z",
    ]);
    let none = BTreeMap::new();
    assert_eq!(
        added_ids(&candidate, &baseline, &none),
        vec!["new_file.py::z", "t.py::c[2]", "t.py::new"]
    );
    // identical lists add nothing, and removing tests adds nothing
    assert!(added_ids(&baseline, &baseline, &none).is_empty());
    assert!(added_ids(&set(&["t.py::a"]), &baseline, &none).is_empty());
    // everything is added when the baseline has no tests
    assert_eq!(added_ids(&baseline, &set(&[]), &none).len(), 4);
}

#[test]
fn tests_in_a_moved_file_are_not_added() {
    let baseline = set(&["old/t.py::a", "old/t.py::T::b[1]", "keep.py::c"]);
    let candidate = set(&[
        "new/t.py::a",
        "new/t.py::T::b[1]",
        // written in the moved file after it moved
        "new/t.py::fresh",
        "keep.py::c",
    ]);
    let moved: BTreeMap<String, String> = [("new/t.py".to_owned(), "old/t.py".to_owned())].into();
    assert_eq!(
        added_ids(&candidate, &baseline, &moved),
        vec!["new/t.py::fresh"]
    );
    // without the rename they all look added
    assert_eq!(added_ids(&candidate, &baseline, &BTreeMap::new()).len(), 3);
}

#[test]
fn ids_are_split_into_batches_that_fit_an_argument_list() {
    let ids: Vec<String> = (0..300)
        .map(|n| format!("tests/test_a.py::{}", "t".repeat(1000 + n)))
        .collect();
    let parts = batches(&ids);
    assert!(parts.len() > 1);
    assert_eq!(parts.iter().map(|p| p.len()).sum::<usize>(), ids.len());
    for part in &parts {
        assert!(part.iter().map(|i| i.len() + 1).sum::<usize>() <= 100_000 + 1400);
    }
    assert!(batches(&[]).is_empty());
    assert_eq!(batches(&ids[..1]).len(), 1);
}

#[test]
fn a_batch_never_spans_two_files() {
    let ids: Vec<String> = [
        "tests/a.py::t1",
        "tests/a.py::t2",
        "tests/b.py::t1",
        "tests/c.py::t1",
        "tests/c.py::t2[x]",
    ]
    .map(str::to_owned)
    .to_vec();
    let parts: Vec<Vec<&str>> = batches(&ids)
        .into_iter()
        .map(|p| p.iter().map(String::as_str).collect())
        .collect();
    assert_eq!(
        parts,
        vec![
            vec!["tests/a.py::t1", "tests/a.py::t2"],
            vec!["tests/b.py::t1"],
            vec!["tests/c.py::t1", "tests/c.py::t2[x]"],
        ]
    );
}

#[test]
fn the_file_of_an_id_is_what_precedes_the_first_separator() {
    assert_eq!(file_of("tests/a.py::T::t[x::y]"), "tests/a.py");
    assert_eq!(file_of("a.py"), "a.py");
}

#[test]
fn a_case_belongs_to_the_test_function_its_id_names() {
    let cases = [
        ("t.py::test_x", ("t.py", "test_x")),
        ("t.py::test_x[1]", ("t.py", "test_x")),
        // a test inherited from a base class is one function
        ("t.py::TestA::test_x[a-b]", ("t.py", "test_x")),
        ("t.py::TestB::test_x[a-b]", ("t.py", "test_x")),
        // parameters may hold anything, brackets and colons too
        ("t.py::test_x[a[0]::b]", ("t.py", "test_x")),
        ("t.py", ("t.py", "")),
    ];
    for (id, want) in cases {
        assert_eq!(function_of(id), want, "{id}");
    }
}
