//! `emi explain`: why a function carries graded erosion and the small
//! structural steps that lower it.
//!
//! Graded erosion (profile `graded`) is `s · [cognitive term + volume
//! term] / 2`, where each term saturates with its density per line. The
//! cognitive term is the part a refactor can change without deleting
//! code: SonarSource cognitive complexity charges every branch 1 plus its
//! nesting level, so nesting is usually the largest avoidable cost. The
//! volume term tracks how much code there is; splitting a function keeps
//! its density and so its mass, which is why splitting alone does not
//! lower graded erosion (unlike threshold warnings, which it silences).

use serde_json::{Value, json};

use crate::{Function, Increment, Lang, functions};

/// Cognitive- and volume-density scales, frozen by study G1 (as
/// `emi_ir::burden`, repeated so this crate stays grammar-only).
const K_COGNITIVE: f64 = 0.071_428_571_428_571_42;
const K_VOLUME: f64 = 15.422_146_593_334_798;

/// Graded erosion split into its two halves.
#[derive(Clone, Debug, PartialEq)]
pub struct Mass {
    pub total: f64,
    /// From cognitive-complexity density: what restructuring can lower.
    pub cognitive_part: f64,
    /// From Halstead-volume density: mostly the amount of code.
    pub volume_part: f64,
}

pub fn mass(sloc: u32, cognitive: u32, volume: f64) -> Mass {
    let s = f64::from(sloc).max(1.0);
    let g = s * (1.0 - (-(f64::from(cognitive) / s) / K_COGNITIVE).exp());
    let v = s * (1.0 - (-(volume / s) / K_VOLUME).exp());
    Mass {
        total: (g + v) / 2.0,
        cognitive_part: g / 2.0,
        volume_part: v / 2.0,
    }
}

/// Where a function's cognitive complexity comes from.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Drivers {
    /// 1 per branch, loop, catch, switch or conditional expression.
    pub structures: u32,
    /// The extra cost of structures nested inside others.
    pub nesting: u32,
    pub boolean_sequences: u32,
    /// `else if`, `elif` and `else` branches.
    pub else_branches: u32,
    pub jumps: u32,
}

pub fn drivers(increments: &[Increment]) -> Drivers {
    let mut d = Drivers::default();
    for i in increments {
        match i.reason.as_str() {
            "boolean sequence" => d.boolean_sequences += i.cost,
            "else if" | "else" => d.else_branches += i.cost,
            "labelled jump" | "goto" => d.jumps += i.cost,
            _ => {
                d.structures += 1;
                d.nesting += i.cost.saturating_sub(1);
            }
        }
    }
    d
}

/// One concrete step, anchored to lines.
#[derive(Clone, Debug, PartialEq)]
pub struct Step {
    pub kind: &'static str,
    pub lines: Vec<u32>,
    pub advice: String,
    /// Cognitive complexity the step removes if carried out as described.
    pub saves: u32,
}

/// The steps that lower this function's cognitive complexity, largest first.
pub fn steps(f: &Function) -> Vec<Step> {
    let mut out = Vec::new();
    let deep: Vec<&Increment> = f
        .increments
        .iter()
        .filter(|i| {
            i.nesting >= 2 && !matches!(i.reason.as_str(), "boolean sequence" | "else if" | "else")
        })
        .collect();
    if let Some(first) = deep.first() {
        let saves: u32 = deep.iter().map(|i| i.nesting).sum();
        out.push(Step {
            kind: "deep nesting",
            lines: deep.iter().map(|i| i.line).collect(),
            advice: format!(
                "structures nested two or more levels deep (first at line {}): return early from the enclosing \
                 conditions (guard clauses), or extract the innermost block into a function of its own",
                first.line
            ),
            saves,
        });
    }
    let shallow: Vec<&Increment> = f
        .increments
        .iter()
        .filter(|i| {
            i.nesting == 1 && !matches!(i.reason.as_str(), "boolean sequence" | "else if" | "else")
        })
        .collect();
    if shallow.len() >= 2 {
        out.push(Step {
            kind: "nested branches",
            lines: shallow.iter().map(|i| i.line).collect(),
            advice: "branches inside a branch or loop: invert the outer condition and continue or return \
                     early, so the inner branches sit at the top level"
                .to_owned(),
            saves: u32::try_from(shallow.len()).unwrap_or(u32::MAX),
        });
    }
    let conditions: Vec<&Increment> = f
        .increments
        .iter()
        .filter(|i| i.reason == "boolean sequence" && i.cost >= 2)
        .collect();
    if !conditions.is_empty() {
        out.push(Step {
            kind: "complex condition",
            lines: conditions.iter().map(|i| i.line).collect(),
            advice:
                "mixed `and`/`or` chains: extract each condition into a named predicate or variable"
                    .to_owned(),
            saves: conditions.iter().map(|i| i.cost - 1).sum(),
        });
    }
    let chain: Vec<&Increment> = f
        .increments
        .iter()
        .filter(|i| i.reason == "else if")
        .collect();
    if chain.len() >= 3 {
        out.push(Step {
            kind: "else-if chain",
            lines: chain.iter().map(|i| i.line).collect(),
            advice: format!(
                "{} `else if` branches: map the cases in a table (dict, map, match) and dispatch once",
                chain.len()
            ),
            saves: u32::try_from(chain.len()).unwrap_or(u32::MAX),
        });
    }
    out.sort_by_key(|s| std::cmp::Reverse(s.saves));
    out
}

/// One function's explanation.
#[derive(Clone, Debug, PartialEq)]
pub struct Explained {
    pub function: Function,
    pub mass: Mass,
    pub drivers: Drivers,
    pub steps: Vec<Step>,
    /// Graded erosion if every step's saving were realised (volume unchanged).
    pub mass_after_steps: f64,
    /// Cognitive complexity if every step's saving were realised.
    pub cognitive_after_steps: u32,
    /// The cognitive term is saturated (about one increment per five lines
    /// or more): graded erosion barely moves until the density falls below
    /// that, so progress is measured in cognitive complexity.
    pub saturated: bool,
}

/// Cognitive density per line above which the cognitive term is within
/// 5% of its ceiling (`1 - e^(-d/k) >= 0.95` at `d = 3k`).
pub const SATURATION_DENSITY: f64 = 3.0 * K_COGNITIVE;

/// Every function of `src`, highest graded erosion first.
pub fn explain(lang: Lang, src: &[u8]) -> Vec<Explained> {
    let mut out: Vec<Explained> = functions(lang, src)
        .into_iter()
        .map(|f| {
            let m = mass(f.sloc, f.cognitive, f.halstead_volume);
            let steps = steps(&f);
            let saved: u32 = steps.iter().map(|s| s.saves).sum();
            let cognitive_after = f.cognitive.saturating_sub(saved);
            let after = mass(f.sloc, cognitive_after, f.halstead_volume).total;
            let saturated = f64::from(f.cognitive) / f64::from(f.sloc.max(1)) >= SATURATION_DENSITY;
            Explained {
                cognitive_after_steps: cognitive_after,
                saturated,
                drivers: drivers(&f.increments),
                mass: m,
                steps,
                mass_after_steps: after,
                function: f,
            }
        })
        .collect();
    out.sort_by(|a, b| b.mass.total.total_cmp(&a.mass.total));
    out
}

/// One explained function as JSON (`emi explain --format json`, MCP).
pub fn record(file: &str, e: &Explained) -> Value {
    let f = &e.function;
    json!({
        "file": file, "function": f.name, "start": f.start, "end": f.end, "sloc": f.sloc,
        "cognitive": f.cognitive, "halstead_volume": f.halstead_volume,
        "graded": {"total": e.mass.total, "complexity": e.mass.cognitive_part, "size": e.mass.volume_part},
        "drivers": {"structures": e.drivers.structures, "nesting": e.drivers.nesting,
                    "boolean_sequences": e.drivers.boolean_sequences,
                    "else_branches": e.drivers.else_branches, "jumps": e.drivers.jumps},
        "steps": e.steps.iter().map(|s| json!({"kind": s.kind, "lines": s.lines, "advice": s.advice,
                                                 "saves_cognitive": s.saves})).collect::<Vec<_>>(),
        "graded_after_steps": e.mass_after_steps,
        "cognitive_after_steps": e.cognitive_after_steps,
        "saturated": e.saturated,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const NESTED: &str = "def handle(items, strict):\n    for item in items:\n        if item.ok:\n            if strict and item.value > 0 or item.forced:\n                for part in item.parts:\n                    if part:\n                        process(part)\n    return None\n";

    #[test]
    fn increments_add_up_to_cognitive_complexity() {
        for f in functions(Lang::Python, NESTED.as_bytes()) {
            assert_eq!(
                f.increments.iter().map(|i| i.cost).sum::<u32>(),
                f.cognitive
            );
        }
    }

    #[test]
    fn nesting_is_named_as_the_main_driver_and_the_fix_lowers_mass() {
        let e = explain(Lang::Python, NESTED.as_bytes()).remove(0);
        // for +1, if +2, if +3, `and`/`or` +2, for +4, if +5
        assert_eq!(e.function.cognitive, 17);
        assert_eq!(e.drivers.nesting, 10);
        assert_eq!(e.steps[0].kind, "deep nesting");
        assert!(e.steps.iter().any(|s| s.kind == "complex condition"));
        assert!(e.mass_after_steps < e.mass.total);
        assert!((e.mass.cognitive_part + e.mass.volume_part - e.mass.total).abs() < 1e-12);
    }

    #[test]
    fn flat_code_gets_no_steps() {
        let e = explain(Lang::Python, b"def f(a):\n    b = a + 1\n    return b\n").remove(0);
        assert!(e.steps.is_empty());
        assert_eq!(e.drivers, Drivers::default());
    }

    #[test]
    fn splitting_evenly_does_not_lower_mass() {
        // the same structure twice in one function, or once in each of two
        let one = "def f(a, b):\n    if a:\n        x = a + 1\n    if b:\n        y = b + 1\n    return a\n";
        let two = "def f(a):\n    if a:\n        x = a + 1\n    return a\n\n\ndef g(b):\n    if b:\n        y = b + 1\n    return b\n";
        let m1: f64 = explain(Lang::Python, one.as_bytes())
            .iter()
            .map(|e| e.mass.total)
            .sum();
        let m2: f64 = explain(Lang::Python, two.as_bytes())
            .iter()
            .map(|e| e.mass.total)
            .sum();
        assert!(m2 >= m1 * 0.9, "{m2} vs {m1}");
    }
}
