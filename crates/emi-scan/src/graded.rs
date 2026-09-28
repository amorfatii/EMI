//! Profile `graded`: function erosion from the lab panel's metrics (study
//! G2). A region whose definition line starts a panel function (the
//! outermost one, if several start there) gets
//! `burden::graded_mass(sloc, cognitive, volume)` of that function. Other
//! regions (module leftovers, declarations) and regions on unparseable
//! lines keep the adapter's erosion. As in G2, a function's panel metrics
//! leave out nested functions.

use emi_ir::burden;
use emi_ir::model::{Evidence, FunctionRecord, RegionRecord};

/// Appended to the adapter's measurement profile.
pub const SUFFIX: &str = "+graded-g2";

/// Re-weigh the regions of one file in place and return the file's new
/// known erosion (the sum over regions with known erosion, in order).
pub fn apply(
    rel: &str,
    source: &str,
    functions: &mut [FunctionRecord],
    regions: &mut [RegionRecord],
) -> f64 {
    let panel = emi_panel::Lang::of_path(rel)
        .map(|lang| emi_panel::functions(lang, source.as_bytes()))
        .unwrap_or_default();
    let mut total = 0.0;
    for (f, r) in functions.iter_mut().zip(regions.iter_mut()) {
        let Some(old) = f.erosion_mass else {
            continue;
        };
        let outermost = panel
            .iter()
            .filter(|p| p.start == f.line && !f.function.starts_with("<module"))
            .max_by_key(|p| p.end);
        let mass = outermost.map_or(old, |p| {
            burden::graded_mass(p.sloc, p.cognitive, p.halstead_volume)
        });
        f.erosion_mass = Some(mass);
        if matches!(r.erosion, Evidence::Exact(_)) {
            r.erosion = Evidence::Exact(mass);
        }
        total += mass;
    }
    total
}
