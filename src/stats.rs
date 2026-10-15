//! Counting statistics: exact Poisson and binomial intervals, and the area
//! of a microscope high-power field.

use serde::{Deserialize, Serialize};
use statrs::distribution::{Beta, ChiSquared, ContinuousCDF};

/// Exact (Garwood) 95 % interval for a Poisson count `k`.
pub fn poisson_interval(k: u64) -> (f64, f64) {
    let alpha = 0.05;
    let k = k as f64;
    let upper = ChiSquared::new(2.0 * (k + 1.0))
        .map(|d| 0.5 * d.inverse_cdf(1.0 - alpha / 2.0))
        .unwrap_or(f64::NAN);
    if k == 0.0 {
        return (0.0, upper);
    }
    let lower = ChiSquared::new(2.0 * k)
        .map(|d| 0.5 * d.inverse_cdf(alpha / 2.0))
        .unwrap_or(f64::NAN);
    (lower, upper)
}

/// Exact (Clopper–Pearson) 95 % interval for a proportion `k / n`.
pub fn binomial_interval(k: u64, n: u64) -> Option<(f64, f64)> {
    if n == 0 || k > n {
        return None;
    }
    let alpha = 0.05;
    let (kf, nf) = (k as f64, n as f64);
    let lower = if k == 0 {
        0.0
    } else {
        Beta::new(kf, nf - kf + 1.0)
            .map(|d| d.inverse_cdf(alpha / 2.0))
            .ok()?
    };
    let upper = if k == n {
        1.0
    } else {
        Beta::new(kf + 1.0, nf - kf)
            .map(|d| d.inverse_cdf(1.0 - alpha / 2.0))
            .ok()?
    };
    Some((lower, upper))
}

/// How the size of one high-power field is defined.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(tag = "mode", rename_all = "snake_case")]
pub enum FieldSize {
    /// Eyepiece field number (mm) and objective magnification: the field
    /// diameter on the specimen is `field_number / objective`.
    FieldNumber { field_number: f64, objective: f64 },
    /// A circular field of the given area.
    Area { mm2: f64 },
}

impl Default for FieldSize {
    /// Field number 22 at 40×: 0.2376 mm², the old fixed value.
    fn default() -> Self {
        FieldSize::FieldNumber {
            field_number: 22.0,
            objective: 40.0,
        }
    }
}

impl FieldSize {
    /// Field area in mm² (`None` when the parameters are unusable).
    pub fn area_mm2(&self) -> Option<f64> {
        let area = match *self {
            FieldSize::FieldNumber {
                field_number,
                objective,
            } => {
                if !(field_number > 0.0 && objective > 0.0) {
                    return None;
                }
                let d = field_number / objective;
                std::f64::consts::PI * (d / 2.0).powi(2)
            }
            FieldSize::Area { mm2 } => mm2,
        };
        (area.is_finite() && area > 0.0 && area < 1_000.0).then_some(area)
    }

    /// Field diameter in mm.
    pub fn diameter_mm(&self) -> Option<f64> {
        self.area_mm2()
            .map(|a| 2.0 * (a / std::f64::consts::PI).sqrt())
    }

    pub fn describe(&self) -> String {
        let Some(area) = self.area_mm2() else {
            return "Invalid field size".to_string();
        };
        let d = self.diameter_mm().unwrap_or(0.0);
        match *self {
            FieldSize::FieldNumber {
                field_number,
                objective,
            } => format!(
                "1 HPF = {area:.4} mm² (field number {} at {}×, ⌀ {d:.3} mm)",
                trim_number(field_number),
                trim_number(objective)
            ),
            FieldSize::Area { .. } => format!("1 HPF = {area:.4} mm² (⌀ {d:.3} mm)"),
        }
    }
}

/// `22` rather than `22.0`, `26.5` stays.
pub fn trim_number(value: f64) -> String {
    let text = format!("{value:.3}");
    text.trim_end_matches('0').trim_end_matches('.').to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn poisson_interval_matches_tables() {
        let (lo, hi) = poisson_interval(10);
        assert!((lo - 4.795).abs() < 1e-3 && (hi - 18.390).abs() < 1e-3);
        let (lo, hi) = poisson_interval(0);
        assert_eq!(lo, 0.0);
        assert!((hi - 3.689).abs() < 1e-3);
    }

    #[test]
    fn binomial_interval_matches_tables() {
        // 5 of 20: Clopper–Pearson 0.0866–0.4910.
        let (lo, hi) = binomial_interval(5, 20).unwrap();
        assert!((lo - 0.0866).abs() < 1e-3, "{lo}");
        assert!((hi - 0.4910).abs() < 1e-3, "{hi}");
        assert_eq!(binomial_interval(0, 10).unwrap().0, 0.0);
        assert_eq!(binomial_interval(10, 10).unwrap().1, 1.0);
        assert_eq!(binomial_interval(1, 0), None);
    }

    #[test]
    fn hpf_area() {
        let default = FieldSize::default();
        assert!((default.area_mm2().unwrap() - 0.2376).abs() < 1e-4);
        assert!(default.describe().contains("field number 22 at 40×"));
        let custom = FieldSize::Area { mm2: 0.5 };
        assert_eq!(custom.area_mm2(), Some(0.5));
        assert!(
            FieldSize::FieldNumber {
                field_number: 0.0,
                objective: 40.0
            }
            .area_mm2()
            .is_none()
        );
        assert_eq!(trim_number(26.5), "26.5");
        assert_eq!(trim_number(22.0), "22");
    }
}
