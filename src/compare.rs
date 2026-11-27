//! Comparing two readers' annotations of the same slide.
//!
//! - **Counts**: points (count points included) and regions per class, for
//!   each reader. Lines, arrows and text are not compared.
//! - **Points**: a point of one reader matches one of the other's of the
//!   same class within a radius (one to one, nearest pairs first); the
//!   agreement is F1 = 2 · matched / (n₁ + n₂).
//! - **Regions**: per class, the union of each reader's areas; the
//!   agreement is the Dice coefficient 2 · |A ∩ B| / (|A| + |B|).

use crate::geometry::{self, Region};
use crate::model::{Color, Geometry};
use std::collections::{BTreeMap, HashMap};

/// One annotation of a reader: its shape and class name.
#[derive(Debug, Clone, PartialEq)]
pub struct Item {
    pub geometry: Geometry,
    pub class: Option<String>,
}

/// What a line of the comparison is about.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Subject {
    /// The class `Row::name`.
    Class,
    /// Annotations without a class.
    Unclassified,
    /// Every region of either reader, whatever its class.
    AllRegions,
}

/// One line of the comparison (a class, or all regions together).
#[derive(Debug, Clone, PartialEq)]
pub struct Row {
    /// The class name (in English for the two rows that are not a class:
    /// the interface names them by `subject`).
    pub name: String,
    pub subject: Subject,
    pub color: Option<Color>,
    /// Point counts: (this reader, the other).
    pub points: (usize, usize),
    /// Points matched between the readers.
    pub matched: usize,
    /// Region counts: (this reader, the other).
    pub regions: (usize, usize),
    /// Areas in level-0 pixels: (this reader, the other, their overlap).
    pub areas: (f64, f64, f64),
}

impl Row {
    pub fn point_agreement(&self) -> Option<f64> {
        let total = self.points.0 + self.points.1;
        (total > 0).then(|| 2.0 * self.matched as f64 / total as f64)
    }

    pub fn dice(&self) -> Option<f64> {
        let total = self.areas.0 + self.areas.1;
        (total > 0.0).then(|| 2.0 * self.areas.2 / total)
    }
}

pub const UNCLASSIFIED: &str = "Unclassified";
/// The last row: every region of either reader, whatever its class.
pub const ALL_REGIONS: &str = "All regions";

/// Points of the two lists matched one to one within `radius`, nearest
/// pairs first.
pub fn match_points(a: &[[f64; 2]], b: &[[f64; 2]], radius: f64) -> usize {
    if a.is_empty() || b.is_empty() || radius <= 0.0 {
        return 0;
    }
    // A grid of `radius`-sized cells over b.
    let cell = |p: &[f64; 2]| {
        (
            (p[0] / radius).floor() as i64,
            (p[1] / radius).floor() as i64,
        )
    };
    let mut grid: HashMap<(i64, i64), Vec<usize>> = HashMap::new();
    for (i, p) in b.iter().enumerate() {
        grid.entry(cell(p)).or_default().push(i);
    }
    let mut pairs: Vec<(f64, usize, usize)> = Vec::new();
    for (i, p) in a.iter().enumerate() {
        let (cx, cy) = cell(p);
        for dx in -1..=1 {
            for dy in -1..=1 {
                for &j in grid.get(&(cx + dx, cy + dy)).into_iter().flatten() {
                    let d = geometry::distance(*p, b[j]);
                    if d <= radius {
                        pairs.push((d, i, j));
                    }
                }
            }
        }
    }
    pairs.sort_by(|x, y| x.0.total_cmp(&y.0));
    let (mut used_a, mut used_b) = (vec![false; a.len()], vec![false; b.len()]);
    let mut matched = 0;
    for (_, i, j) in pairs {
        if !used_a[i] && !used_b[j] {
            used_a[i] = true;
            used_b[j] = true;
            matched += 1;
        }
    }
    matched
}

/// Union area of `ours`, of `theirs`, and of their overlap.
fn region_areas(ours: Vec<Geometry>, theirs: Vec<Geometry>) -> (f64, f64, f64) {
    let a = Region::new(ours.clone()).area_px();
    let b = Region::new(theirs.clone()).area_px();
    if a == 0.0 || b == 0.0 {
        return (a, b, 0.0);
    }
    let mut both = ours;
    both.extend(theirs);
    let union = Region::new(both).area_px();
    (a, b, (a + b - union).max(0.0))
}

/// Compares two readers. Rows: one per class (in `order`, then any other
/// class either reader used, then unclassified), and "All regions".
pub fn compare(
    ours: &[Item],
    theirs: &[Item],
    order: &[(String, Color)],
    match_radius: f64,
) -> Vec<Row> {
    let name = |item: &Item| {
        item.class
            .clone()
            .unwrap_or_else(|| UNCLASSIFIED.to_string())
    };
    let mut names: Vec<String> = order.iter().map(|(n, _)| n.clone()).collect();
    for item in ours.iter().chain(theirs) {
        let n = name(item);
        if !names.iter().any(|m| m.eq_ignore_ascii_case(&n)) {
            names.push(n);
        }
    }
    let key = |n: &str| n.to_lowercase();
    type Split = (Vec<[f64; 2]>, Vec<Geometry>);
    let split = |items: &[Item]| {
        let mut by: BTreeMap<String, Split> = BTreeMap::new();
        for item in items {
            let entry = by.entry(key(&name(item))).or_default();
            match &item.geometry {
                Geometry::Point { x, y } => entry.0.push([*x, *y]),
                g if g.kind().is_area() => entry.1.push(g.clone()),
                _ => {}
            }
        }
        by
    };
    let (mine, other) = (split(ours), split(theirs));
    let empty: Split = (Vec::new(), Vec::new());
    let mut rows = Vec::new();
    for n in names {
        let (a, b) = (
            mine.get(&key(&n)).unwrap_or(&empty),
            other.get(&key(&n)).unwrap_or(&empty),
        );
        if a.0.is_empty() && b.0.is_empty() && a.1.is_empty() && b.1.is_empty() {
            continue;
        }
        rows.push(Row {
            color: order
                .iter()
                .find(|(m, _)| m.eq_ignore_ascii_case(&n))
                .map(|(_, c)| *c),
            subject: if n == UNCLASSIFIED {
                Subject::Unclassified
            } else {
                Subject::Class
            },
            name: n,
            points: (a.0.len(), b.0.len()),
            matched: match_points(&a.0, &b.0, match_radius),
            regions: (a.1.len(), b.1.len()),
            areas: region_areas(a.1.clone(), b.1.clone()),
        });
    }
    let all = |by: &BTreeMap<String, Split>| {
        by.values()
            .flat_map(|(_, r)| r.iter().cloned())
            .collect::<Vec<_>>()
    };
    let (a, b) = (all(&mine), all(&other));
    if !a.is_empty() || !b.is_empty() {
        rows.push(Row {
            name: ALL_REGIONS.to_string(),
            subject: Subject::AllRegions,
            color: None,
            points: (0, 0),
            matched: 0,
            regions: (a.len(), b.len()),
            areas: region_areas(a, b),
        });
    }
    rows
}

#[cfg(test)]
mod tests {
    use super::*;

    fn square(x: f64, size: f64, class: &str) -> Item {
        Item {
            geometry: Geometry::Rectangle {
                x,
                y: 0.0,
                width: size,
                height: size,
            },
            class: Some(class.to_string()),
        }
    }

    fn point(x: f64, y: f64, class: Option<&str>) -> Item {
        Item {
            geometry: Geometry::Point { x, y },
            class: class.map(str::to_string),
        }
    }

    #[test]
    fn points_match_one_to_one_nearest_first() {
        let a = [[0.0, 0.0], [10.0, 0.0], [100.0, 100.0]];
        let b = [[1.0, 0.0], [9.0, 0.0], [10.5, 0.0], [500.0, 500.0]];
        assert_eq!(match_points(&a, &b, 2.0), 2);
        assert_eq!(match_points(&a, &b, 0.1), 0);
        assert_eq!(match_points(&a, &[], 2.0), 0);
    }

    #[test]
    fn readers_are_compared_per_class_by_counts_and_overlap() {
        let order = vec![
            ("Tumour".to_string(), Color(1, 2, 3)),
            ("Mitosis".to_string(), Color(4, 5, 6)),
        ];
        let ours = vec![
            square(0.0, 100.0, "Tumour"),
            point(10.0, 10.0, Some("Mitosis")),
            point(50.0, 50.0, Some("Mitosis")),
            point(5.0, 5.0, None),
        ];
        let theirs = vec![
            // Overlaps half of ours.
            square(50.0, 100.0, "tumour"),
            point(11.0, 10.0, Some("Mitosis")),
            point(300.0, 300.0, Some("Mitosis")),
            point(90.0, 90.0, Some("Mitosis")),
        ];
        let rows = compare(&ours, &theirs, &order, 5.0);
        let names: Vec<&str> = rows.iter().map(|r| r.name.as_str()).collect();
        assert_eq!(names, ["Tumour", "Mitosis", "Unclassified", "All regions"]);
        let tumour = &rows[0];
        assert_eq!(tumour.color, Some(Color(1, 2, 3)));
        assert!((tumour.areas.2 - 5000.0).abs() < 1e-6, "{:?}", tumour.areas);
        assert!((tumour.dice().unwrap() - 0.5).abs() < 1e-9);
        assert_eq!(tumour.regions, (1, 1));
        assert_eq!(tumour.points, (0, 0));
        assert_eq!(tumour.point_agreement(), None);
        let mitosis = &rows[1];
        assert_eq!(mitosis.regions, (0, 0));
        assert_eq!(mitosis.dice(), None);
        assert_eq!(mitosis.points, (2, 3));
        assert_eq!(mitosis.matched, 1);
        assert!((mitosis.point_agreement().unwrap() - 0.4).abs() < 1e-9);
        assert_eq!(rows[2].points, (1, 0));
        assert_eq!(rows[2].point_agreement(), Some(0.0));
        assert!((rows[3].dice().unwrap() - 0.5).abs() < 1e-9);
    }
}
