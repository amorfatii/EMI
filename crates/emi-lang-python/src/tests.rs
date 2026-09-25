use super::*;

fn table(source: &str) -> Vec<(String, u32, u32, u32)> {
    let m = measure("m.py", source)
        .map_err(|e| e.message)
        .unwrap_or_else(|e| panic!("{e}"));
    m.functions
        .into_iter()
        .map(|f| (f.function, f.line, f.sloc, f.cc.unwrap_or(0)))
        .collect()
}

#[test]
fn docstrings_are_not_source_and_regions_nest() {
    let src = "\"\"\"Module doc.\"\"\"\nimport os\n\n\n@dec\ndef outer(a):\n    \"\"\"Doc\n    spans lines.\"\"\"\n    def inner(b):\n        return b if b else a\n    return inner\n";
    assert_eq!(
        table(src),
        vec![
            ("inner".into(), 9, 2, 2),
            ("outer".into(), 6, 3, 1),
            ("<module/class blocks>".into(), 1, 1, 1),
        ]
    );
}

#[test]
fn complexity_counts_match_cpython_rules() {
    let src = concat!(
        "def f(x, ys):\n",
        "    if x and ys or not x:\n",
        "        pass\n",
        "    elif x:\n",
        "        pass\n",
        "    else:\n",
        "        pass\n",
        "    for y in ys:\n",
        "        assert y\n",
        "    while x:\n",
        "        x -= 1\n",
        "    try:\n",
        "        pass\n",
        "    except ValueError:\n",
        "        pass\n",
        "    z = [a for a in ys if a if a > 1 for b in ys]\n",
        "    match x:\n",
        "        case 1:\n",
        "            pass\n",
        "        case _:\n",
        "            pass\n",
        "    return sum(a for a in ys)\n",
    );
    // 1 + if(1) + and/or(2) + elif(1) + for(1) + assert(1) + while(1)
    //   + except(1) + listcomp(1+2 + 1+0) + match(1) + genexp(1) = 15
    assert_eq!(table(src)[0].3, 15);
}

#[test]
fn overload_stubs_and_semicolons_are_not_source() {
    // Expected values from the v0.5.1 oracle (tools/oracle_snippet.py).
    let src = "from typing import overload\n@overload\ndef f(x: int) -> int: ...\n@overload\ndef f(x: str) -> str: ...\ndef f(x):\n    return x;\nx = 1;\n";
    let got = table(src);
    assert_eq!(got[0], ("f".into(), 6, 2, 1));
    assert_eq!(got[1], ("<module/class blocks>".into(), 1, 2, 1));
}

#[test]
fn async_def_line_and_class_qualified_ids() {
    let src = "class C:\n    @staticmethod\n    async def m():\n        return 1\n";
    let m = measure("m.py", src)
        .map_err(|e| e.message)
        .unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(m.functions[0].line, 3);
    assert_eq!(m.functions[0].sloc, 3);
    assert_eq!(m.regions[0].id, "C.m");
    assert_eq!(m.regions[1].id, "<module>");
}
