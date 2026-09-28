//! Burden formulas of calculator profile `emi-0.3` (v0.5 `emi.py:36-72`).
//! Operation order is kept identical so results match the oracle bit for bit.

/// Saturating excess above `threshold`, in [0, 1).
pub fn severity(value: f64, threshold: f64, scale: f64) -> f64 {
    let excess = (value - threshold).max(0.0);
    excess / (excess + scale)
}

/// Erosion mass of one region: lines weighted by the worse of branch-count
/// severity (CC above 10) and size severity (SLOC above 50).
pub fn erosion_mass(sloc: u32, cc: u32) -> f64 {
    let loc = f64::from(sloc);
    let cc = f64::from(cc);
    loc * severity(cc, 10.0, 20.0).max(severity(loc, 50.0, 100.0))
}

/// Cognitive-complexity scale of the `graded` erosion, frozen by study G1
/// (emi-lab `results/G1/results.json`, k_g) and confirmed by G2.
pub const GRADED_K_COGNITIVE: f64 = 0.071_428_571_428_571_42; // 1/14
/// Halstead-volume scale of the `graded` erosion (G1 k_v).
pub const GRADED_K_VOLUME: f64 = 15.422_146_593_334_798;

/// Graded erosion of one function (profile `graded`): lines weighted by
/// saturating cognitive-complexity density and Halstead-volume density,
/// averaged. `s · [(1 − e^(−(g/s)/k_g)) + (1 − e^(−(v/s)/k_v))] / 2`, with
/// the lab panel's SLOC, cognitive complexity and volume (emi-panel).
/// Operation order follows the lab (`tools/g1_select.py::graded`).
pub fn graded_mass(sloc: u32, cognitive: u32, volume: f64) -> f64 {
    let s = f64::from(sloc).max(1.0);
    let g_cog = s * (1.0 - (-(f64::from(cognitive) / s) / GRADED_K_COGNITIVE).exp());
    let g_vol = s * (1.0 - (-(volume / s) / GRADED_K_VOLUME).exp());
    (g_cog + g_vol) / 2.0
}

/// Coupling mass of one region: cycle-covered lines count fully, the rest
/// are weighted by direct first-party fan-out above 3.
pub fn coupling_mass(sloc: u32, fanout: u32, cycle_sloc: u32) -> f64 {
    let loc = f64::from(sloc);
    let cycles = f64::from(cycle_sloc);
    cycles + (loc - cycles) * severity(f64::from(fanout), 3.0, 6.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn erosion_matches_oracle_values() {
        // Values computed with emi.erosion_burden in the v0.5.1 oracle.
        assert_eq!(erosion_mass(10, 5), 0.0);
        assert_eq!(erosion_mass(20, 30), 20.0 * (20.0 / 40.0));
        assert_eq!(erosion_mass(120, 3), 120.0 * (70.0 / 170.0));
    }

    #[test]
    fn graded_matches_lab_values() {
        // tools/g1_select.graded(sloc, cog, vol, k_g, k_v)["G_mix"]
        assert!((graded_mass(20, 5, 800.0) - 18.950_575_943_686_147).abs() < 1e-12);
        assert_eq!(graded_mass(0, 0, 0.0), 0.0);
    }

    #[test]
    fn coupling_counts_cycles_fully() {
        assert_eq!(coupling_mass(10, 0, 10), 10.0);
        assert_eq!(coupling_mass(12, 9, 0), 12.0 * (6.0 / 12.0));
    }
}
