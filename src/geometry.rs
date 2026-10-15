//! Planar geometry on level-0 pixel coordinates.
//!
//! One fill rule everywhere (ANN-3): the host fills polygons with the
//! even-odd rule, point counting uses even-odd containment, and areas are
//! computed for the same region. Polygons are validated (and repaired with
//! the even-odd rule) when they are placed, edited or imported, so stored
//! polygons are simple rings whose shoelace area equals their filled area.

use crate::model::{Bounds, Geometry, MAX_VERTICES};
use i_overlay::core::fill_rule::FillRule;
use i_overlay::core::overlay_rule::OverlayRule;
use i_overlay::float::simplify::SimplifyShape;
use i_overlay::float::single::SingleFloatOverlay;

pub type P = [f64; 2];
/// Rings of one polygon: exterior first, then holes.
pub type Rings = Vec<Vec<P>>;

/// Segments used for ellipses in boolean operations.
const ELLIPSE_SEGMENTS: usize = 1024;

pub fn distance(a: P, b: P) -> f64 {
    (a[0] - b[0]).hypot(a[1] - b[1])
}

/// Signed shoelace area of a ring (positive when clockwise on screen, i.e.
/// counter-clockwise in y-up coordinates).
pub fn ring_signed_area(ring: &[P]) -> f64 {
    let n = ring.len();
    if n < 3 {
        return 0.0;
    }
    let mut sum = 0.0;
    // Relative to the first vertex for precision far from the origin.
    let o = ring[0];
    for i in 0..n {
        let a = ring[i];
        let b = ring[(i + 1) % n];
        sum += (a[0] - o[0]) * (b[1] - o[1]) - (b[0] - o[0]) * (a[1] - o[1]);
    }
    sum / 2.0
}

pub fn ring_area(ring: &[P]) -> f64 {
    ring_signed_area(ring).abs()
}

/// Area of a valid polygon (simple exterior, disjoint holes inside it).
pub fn polygon_area(exterior: &[P], holes: &[Vec<P>]) -> f64 {
    (ring_area(exterior) - holes.iter().map(|h| ring_area(h)).sum::<f64>()).max(0.0)
}

pub fn polyline_length(points: &[P]) -> f64 {
    points.windows(2).map(|w| distance(w[0], w[1])).sum()
}

pub fn ring_perimeter(ring: &[P]) -> f64 {
    if ring.len() < 2 {
        return 0.0;
    }
    polyline_length(ring) + distance(ring[ring.len() - 1], ring[0])
}

/// Even-odd point-in-ring test.
pub fn point_in_ring(p: P, ring: &[P]) -> bool {
    let n = ring.len();
    if n < 3 {
        return false;
    }
    let mut inside = false;
    let mut j = n - 1;
    for i in 0..n {
        let (xi, yi) = (ring[i][0], ring[i][1]);
        let (xj, yj) = (ring[j][0], ring[j][1]);
        if (yi > p[1]) != (yj > p[1]) && p[0] < (xj - xi) * (p[1] - yi) / (yj - yi) + xi {
            inside = !inside;
        }
        j = i;
    }
    inside
}

/// Even-odd containment over every ring, matching how the host fills the
/// polygon (for valid polygons: inside the exterior and outside the holes).
pub fn point_in_polygon(p: P, exterior: &[P], holes: &[Vec<P>]) -> bool {
    holes
        .iter()
        .fold(point_in_ring(p, exterior), |inside, hole| {
            inside ^ point_in_ring(p, hole)
        })
}

/// A polygon approximating an ellipse.
pub fn ellipse_ring(cx: f64, cy: f64, rx: f64, ry: f64, segments: usize) -> Vec<P> {
    let n = segments.max(8);
    (0..n)
        .map(|i| {
            let t = i as f64 / n as f64 * std::f64::consts::TAU;
            [cx + rx * t.cos(), cy + ry * t.sin()]
        })
        .collect()
}

/// Whether `p` lies inside an area geometry (polygon, rectangle, ellipse).
pub fn contains(geometry: &Geometry, p: P) -> bool {
    match geometry {
        Geometry::Polygon { exterior, holes } => point_in_polygon(p, exterior, holes),
        Geometry::Rectangle {
            x,
            y,
            width,
            height,
        } => p[0] >= *x && p[0] <= x + width && p[1] >= *y && p[1] <= y + height,
        Geometry::Ellipse { cx, cy, rx, ry } => {
            if *rx <= 0.0 || *ry <= 0.0 {
                return false;
            }
            let dx = (p[0] - cx) / rx;
            let dy = (p[1] - cy) / ry;
            dx * dx + dy * dy <= 1.0
        }
        _ => false,
    }
}

/// Filled area of an area geometry in px², with the even-odd rule (exact for
/// rectangles and ellipses; invalid polygons are resolved before measuring).
pub fn area(geometry: &Geometry) -> f64 {
    match geometry {
        Geometry::Polygon { exterior, holes } => {
            let formula = polygon_area(exterior, holes);
            match polygon_validity(exterior, holes) {
                Validity::Valid => formula,
                _ => even_odd_area(exterior, holes),
            }
        }
        Geometry::Rectangle { width, height, .. } => width.abs() * height.abs(),
        Geometry::Ellipse { rx, ry, .. } => std::f64::consts::PI * rx.abs() * ry.abs(),
        _ => 0.0,
    }
}

/// Area filled by the even-odd rule over all rings.
pub fn even_odd_area(exterior: &[P], holes: &[Vec<P>]) -> f64 {
    let mut rings: Rings = Vec::with_capacity(1 + holes.len());
    rings.push(exterior.to_vec());
    rings.extend(holes.iter().cloned());
    shapes_area(&rings.simplify_shape(FillRule::EvenOdd))
}

/// Total area of overlay output shapes.
pub fn shapes_area(shapes: &[Rings]) -> f64 {
    shapes
        .iter()
        .map(|rings| match rings.split_first() {
            Some((exterior, holes)) => polygon_area(exterior, holes),
            None => 0.0,
        })
        .sum()
}

/// Removes consecutive duplicate vertices and a repeated closing vertex.
pub fn dedup_ring(ring: &[P]) -> Vec<P> {
    let mut out: Vec<P> = Vec::with_capacity(ring.len());
    for p in ring {
        if out.last().is_none_or(|last| distance(*last, *p) > 1e-9) {
            out.push(*p);
        }
    }
    while out.len() > 1 && distance(out[0], out[out.len() - 1]) <= 1e-9 {
        out.pop();
    }
    out
}

fn dedup_line(points: &[P]) -> Vec<P> {
    let mut out: Vec<P> = Vec::with_capacity(points.len());
    for p in points {
        if out.last().is_none_or(|last| distance(*last, *p) > 1e-9) {
            out.push(*p);
        }
    }
    out
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Validity {
    Valid,
    /// Self-intersecting, holes outside or overlapping, or several parts.
    Invalid,
    /// No area at all.
    Empty,
}

/// Whether a polygon is a single simple region whose shoelace area is its
/// even-odd filled area.
///
/// A ring crossing itself always creates a face with winding number 2 or
/// −1, which the shoelace formula weighs differently from the even-odd
/// rule, so comparing the two areas (plus the number of resulting parts
/// and rings) detects crossings, holes outside the exterior and
/// overlapping holes without an explicit segment-intersection search.
pub fn polygon_validity(exterior: &[P], holes: &[Vec<P>]) -> Validity {
    if exterior.len() < 3 {
        return Validity::Empty;
    }
    let formula = polygon_area(exterior, holes);
    let mut rings: Rings = Vec::with_capacity(1 + holes.len());
    rings.push(exterior.to_vec());
    rings.extend(holes.iter().cloned());
    let resolved = rings.simplify_shape(FillRule::EvenOdd);
    let resolved_area = shapes_area(&resolved);
    if resolved_area <= f64::EPSILON {
        return Validity::Empty;
    }
    let perimeter: f64 = rings.iter().map(|r| ring_perimeter(r)).sum();
    let tolerance = (resolved_area * 1e-7).max(perimeter * 1e-3).max(1e-6);
    let same_area = (formula - resolved_area).abs() <= tolerance;
    let one_part = resolved.len() == 1 && resolved[0].len() == rings.len();
    if same_area && one_part {
        Validity::Valid
    } else {
        Validity::Invalid
    }
}

/// What normalisation did to a geometry.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Normalized {
    /// One or more parts (a repaired or clipped polygon can fall apart).
    pub parts: Vec<Geometry>,
    /// The outline crossed itself (or its holes were inconsistent) and was
    /// rebuilt with the even-odd rule.
    pub repaired: bool,
    /// Parts outside the slide were cut off.
    pub clipped: bool,
}

/// Validates a geometry for storage and makes it consistent:
///
/// - coordinates must be finite and the vertex count within limits;
/// - points and text must lie on the slide (`image`, when known);
/// - lines need two distinct points (vertices are clamped to the slide);
/// - rectangles and ellipses need a positive size (rectangles are clipped
///   to the slide; ellipses must be centred on it);
/// - polygons need three corners and a non-zero area; self-intersecting
///   outlines are repaired with the even-odd rule (possibly into several
///   parts) and clipped to the slide.
pub fn normalize(geometry: Geometry, image: Option<Bounds>) -> Result<Normalized, String> {
    if !geometry.is_finite() {
        return Err("The shape has invalid (non-finite) coordinates.".to_string());
    }
    if geometry.vertex_count() > MAX_VERTICES {
        return Err(format!(
            "The shape has {} vertices; the limit is {MAX_VERTICES}.",
            geometry.vertex_count()
        ));
    }
    let on_slide = |x: f64, y: f64| image.is_none_or(|b| b.contains(x, y));
    let mut out = Normalized::default();
    match geometry {
        Geometry::Point { x, y } | Geometry::Text { x, y } => {
            if !on_slide(x, y) {
                return Err("The point is outside the slide.".to_string());
            }
            out.parts.push(geometry);
        }
        Geometry::Polyline { points } => {
            let mut points = dedup_line(&points);
            if let Some(b) = image {
                for p in &mut points {
                    let clamped = [p[0].clamp(b.min_x, b.max_x), p[1].clamp(b.min_y, b.max_y)];
                    if clamped != *p {
                        out.clipped = true;
                        *p = clamped;
                    }
                }
                points = dedup_line(&points);
            }
            if points.len() < 2 {
                return Err("A line needs at least two distinct points.".to_string());
            }
            out.parts.push(Geometry::Polyline { points });
        }
        Geometry::Rectangle {
            mut x,
            mut y,
            mut width,
            mut height,
        } => {
            if width < 0.0 {
                x += width;
                width = -width;
            }
            if height < 0.0 {
                y += height;
                height = -height;
            }
            if let Some(b) = image {
                let (x0, y0) = (x.max(b.min_x), y.max(b.min_y));
                let (x1, y1) = ((x + width).min(b.max_x), (y + height).min(b.max_y));
                if x1 <= x0 || y1 <= y0 {
                    return Err("The rectangle is outside the slide.".to_string());
                }
                if (x0, y0, x1, y1) != (x, y, x + width, y + height) {
                    out.clipped = true;
                }
                (x, y, width, height) = (x0, y0, x1 - x0, y1 - y0);
            }
            if width <= 1e-9 || height <= 1e-9 {
                return Err("The rectangle has no area.".to_string());
            }
            out.parts.push(Geometry::Rectangle {
                x,
                y,
                width,
                height,
            });
        }
        Geometry::Ellipse { cx, cy, rx, ry } => {
            let (rx, ry) = (rx.abs(), ry.abs());
            if rx <= 1e-9 || ry <= 1e-9 {
                return Err("The ellipse has no area.".to_string());
            }
            if !on_slide(cx, cy) {
                return Err("The ellipse is centred outside the slide.".to_string());
            }
            out.parts.push(Geometry::Ellipse { cx, cy, rx, ry });
        }
        Geometry::Polygon { exterior, holes } => {
            let exterior = dedup_ring(&exterior);
            let holes: Vec<Vec<P>> = holes
                .iter()
                .map(|h| dedup_ring(h))
                .filter(|h| h.len() >= 3 && ring_area(h) > 0.0)
                .collect();
            if exterior.len() < 3 {
                return Err("A polygon needs at least three distinct corners.".to_string());
            }
            let mut parts: Vec<Rings> = match polygon_validity(&exterior, &holes) {
                Validity::Empty => return Err("The polygon encloses no area.".to_string()),
                Validity::Valid => {
                    let mut rings = vec![exterior];
                    rings.extend(holes);
                    vec![rings]
                }
                Validity::Invalid => {
                    out.repaired = true;
                    let mut rings = vec![exterior];
                    rings.extend(holes);
                    drop_slivers(rings.simplify_shape(FillRule::EvenOdd))
                }
            };
            if let Some(b) = image {
                let bounds = Bounds::of_points(parts.iter().flat_map(|rings| rings[0].iter()));
                let inside = bounds.min_x >= b.min_x
                    && bounds.min_y >= b.min_y
                    && bounds.max_x <= b.max_x
                    && bounds.max_y <= b.max_y;
                if !inside {
                    let frame = vec![
                        [b.min_x, b.min_y],
                        [b.max_x, b.min_y],
                        [b.max_x, b.max_y],
                        [b.min_x, b.max_y],
                    ];
                    parts = drop_slivers(parts.overlay(
                        &frame,
                        OverlayRule::Intersect,
                        FillRule::EvenOdd,
                    ));
                    out.clipped = true;
                }
            }
            if parts.is_empty() {
                return Err(if out.clipped {
                    "The polygon is outside the slide.".to_string()
                } else {
                    "The polygon encloses no area.".to_string()
                });
            }
            for mut rings in parts {
                let exterior = rings.remove(0);
                out.parts.push(Geometry::Polygon {
                    exterior,
                    holes: rings,
                });
            }
        }
    }
    Ok(out)
}

/// Drops parts below half a square pixel (numerical slivers of overlays).
fn drop_slivers(shapes: Vec<Rings>) -> Vec<Rings> {
    shapes
        .into_iter()
        .filter(|rings| {
            rings
                .split_first()
                .is_some_and(|(exterior, holes)| polygon_area(exterior, holes) >= 0.5)
        })
        .collect()
}

/// The rings of an area geometry for boolean operations.
pub fn to_rings(geometry: &Geometry) -> Option<Rings> {
    match geometry {
        Geometry::Polygon { exterior, holes } => {
            let mut rings = vec![exterior.clone()];
            rings.extend(holes.iter().cloned());
            Some(rings)
        }
        Geometry::Rectangle {
            x,
            y,
            width,
            height,
        } => Some(vec![vec![
            [*x, *y],
            [x + width, *y],
            [x + width, y + height],
            [*x, y + height],
        ]]),
        Geometry::Ellipse { cx, cy, rx, ry } => {
            Some(vec![ellipse_ring(*cx, *cy, *rx, *ry, ELLIPSE_SEGMENTS)])
        }
        _ => None,
    }
}

fn rings_to_geometries(shapes: Vec<Rings>) -> Vec<Geometry> {
    drop_slivers(shapes)
        .into_iter()
        .map(|mut rings| {
            let exterior = rings.remove(0);
            Geometry::Polygon {
                exterior,
                holes: rings,
            }
        })
        .collect()
}

/// `a ∪ b` (both area geometries). `None` if either is not an area.
pub fn union(a: &Geometry, b: &Geometry) -> Option<Vec<Geometry>> {
    let (ra, rb) = (to_rings(a)?, to_rings(b)?);
    Some(rings_to_geometries(ra.overlay(
        &rb,
        OverlayRule::Union,
        FillRule::EvenOdd,
    )))
}

/// `a − b` (both area geometries). `None` if either is not an area.
pub fn difference(a: &Geometry, b: &Geometry) -> Option<Vec<Geometry>> {
    let (ra, rb) = (to_rings(a)?, to_rings(b)?);
    Some(rings_to_geometries(ra.overlay(
        &rb,
        OverlayRule::Difference,
        FillRule::EvenOdd,
    )))
}

/// Orients the exterior positively and holes negatively.
fn oriented(mut rings: Rings) -> Rings {
    for (i, ring) in rings.iter_mut().enumerate() {
        let positive = ring_signed_area(ring) > 0.0;
        if positive != (i == 0) {
            ring.reverse();
        }
    }
    rings
}

/// A counting region: one or more area geometries.
#[derive(Debug, Clone)]
pub struct Region {
    parts: Vec<(Geometry, Bounds)>,
    area_px: f64,
}

impl Region {
    /// The union of `parts` (non-area geometries are ignored).
    pub fn new(parts: Vec<Geometry>) -> Region {
        let parts: Vec<(Geometry, Bounds)> = parts
            .into_iter()
            .filter(|g| g.kind().is_area())
            .map(|g| {
                let b = g.bounds();
                (g, b)
            })
            .collect();
        let overlapping = parts
            .iter()
            .enumerate()
            .any(|(i, (_, a))| parts[i + 1..].iter().any(|(_, b)| a.intersects(b)));
        let area_px = if overlapping {
            // Exact union for overlapping regions: exteriors in one
            // orientation, holes in the other, then the non-zero rule.
            let mut shapes: Vec<Rings> = Vec::new();
            for (g, _) in &parts {
                if let Some(rings) = to_rings(g) {
                    shapes.push(oriented(rings));
                }
            }
            shapes_area(&shapes.simplify_shape(FillRule::NonZero))
        } else {
            parts.iter().map(|(g, _)| area(g)).sum()
        };
        Region { parts, area_px }
    }

    pub fn is_empty(&self) -> bool {
        self.parts.is_empty()
    }

    pub fn len(&self) -> usize {
        self.parts.len()
    }

    pub fn area_px(&self) -> f64 {
        self.area_px
    }

    pub fn bounds(&self) -> Bounds {
        let mut iter = self.parts.iter().map(|(_, b)| *b);
        let Some(first) = iter.next() else {
            return Bounds::default();
        };
        iter.fold(first, |acc, b| Bounds {
            min_x: acc.min_x.min(b.min_x),
            min_y: acc.min_y.min(b.min_y),
            max_x: acc.max_x.max(b.max_x),
            max_y: acc.max_y.max(b.max_y),
        })
    }

    pub fn contains(&self, p: P) -> bool {
        self.parts
            .iter()
            .any(|(g, b)| b.contains(p[0], p[1]) && contains(g, p))
    }
}

/// Convex hull (Andrew's monotone chain), counter-clockwise in y-up terms.
pub fn convex_hull(points: &[P]) -> Vec<P> {
    let mut pts: Vec<P> = points.to_vec();
    pts.sort_by(|a, b| a[0].total_cmp(&b[0]).then(a[1].total_cmp(&b[1])));
    pts.dedup_by(|a, b| a[0] == b[0] && a[1] == b[1]);
    if pts.len() < 3 {
        return pts;
    }
    let cross = |o: P, a: P, b: P| (a[0] - o[0]) * (b[1] - o[1]) - (a[1] - o[1]) * (b[0] - o[0]);
    let mut lower: Vec<P> = Vec::new();
    for p in &pts {
        while lower.len() >= 2 && cross(lower[lower.len() - 2], lower[lower.len() - 1], *p) <= 0.0 {
            lower.pop();
        }
        lower.push(*p);
    }
    let mut upper: Vec<P> = Vec::new();
    for p in pts.iter().rev() {
        while upper.len() >= 2 && cross(upper[upper.len() - 2], upper[upper.len() - 1], *p) <= 0.0 {
            upper.pop();
        }
        upper.push(*p);
    }
    lower.pop();
    upper.pop();
    lower.extend(upper);
    lower
}

/// Maximum and minimum caliper (Feret) diameters of a point set: the
/// largest distance between two points, and the smallest width over all
/// directions.
pub fn calipers(points: &[P]) -> (f64, f64) {
    let hull = convex_hull(points);
    match hull.len() {
        0 | 1 => return (0.0, 0.0),
        2 => return (distance(hull[0], hull[1]), 0.0),
        _ => {}
    }
    let n = hull.len();
    // Maximum: farthest pair (O(n²) on the hull, which is small).
    let mut max_d: f64 = 0.0;
    if n <= 2000 {
        for i in 0..n {
            for j in i + 1..n {
                max_d = max_d.max(distance(hull[i], hull[j]));
            }
        }
    } else {
        // Rotating calipers for very large hulls.
        let mut j = 1;
        for i in 0..n {
            let a = hull[i];
            let b = hull[(i + 1) % n];
            let edge_cross =
                |p: P| ((b[0] - a[0]) * (p[1] - a[1]) - (b[1] - a[1]) * (p[0] - a[0])).abs();
            while edge_cross(hull[(j + 1) % n]) > edge_cross(hull[j]) {
                j = (j + 1) % n;
            }
            max_d = max_d.max(distance(a, hull[j])).max(distance(b, hull[j]));
        }
    }
    // Minimum width: for each hull edge, the farthest hull vertex from it.
    let mut min_w = f64::INFINITY;
    let mut j = 1;
    for i in 0..n {
        let a = hull[i];
        let b = hull[(i + 1) % n];
        let len = distance(a, b);
        if len <= 0.0 {
            continue;
        }
        let height =
            |p: P| ((b[0] - a[0]) * (p[1] - a[1]) - (b[1] - a[1]) * (p[0] - a[0])).abs() / len;
        while height(hull[(j + 1) % n]) > height(hull[j]) + 1e-12 {
            j = (j + 1) % n;
        }
        min_w = min_w.min(height(hull[j]));
    }
    (max_d, if min_w.is_finite() { min_w } else { 0.0 })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn square(x: f64, y: f64, s: f64) -> Vec<P> {
        vec![[x, y], [x + s, y], [x + s, y + s], [x, y + s]]
    }

    fn slide() -> Option<Bounds> {
        Some(Bounds {
            min_x: 0.0,
            min_y: 0.0,
            max_x: 10_000.0,
            max_y: 10_000.0,
        })
    }

    #[test]
    fn areas_and_containment_agree() {
        let l_shape = vec![
            [0.0, 0.0],
            [10.0, 0.0],
            [10.0, 4.0],
            [4.0, 4.0],
            [4.0, 10.0],
            [0.0, 10.0],
        ];
        assert_eq!(polygon_area(&l_shape, &[]), 64.0);
        assert!(point_in_polygon([2.0, 8.0], &l_shape, &[]));
        assert!(!point_in_polygon([8.0, 8.0], &l_shape, &[]));
        let hole = square(40.0, 40.0, 20.0);
        let outer = square(0.0, 0.0, 100.0);
        assert!(point_in_polygon(
            [10.0, 10.0],
            &outer,
            std::slice::from_ref(&hole)
        ));
        assert!(!point_in_polygon(
            [50.0, 50.0],
            &outer,
            std::slice::from_ref(&hole)
        ));
        let g = Geometry::Polygon {
            exterior: outer,
            holes: vec![hole],
        };
        assert_eq!(area(&g), 10_000.0 - 400.0);
    }

    #[test]
    fn bowtie_area_matches_even_odd_counting() {
        // ANN-3: the signed shoelace area of this bowtie is 0 although both
        // lobes are filled on screen.
        let bowtie = vec![[0.0, 0.0], [1000.0, 1000.0], [1000.0, 0.0], [0.0, 1000.0]];
        assert_eq!(polygon_area(&bowtie, &[]), 0.0);
        let g = Geometry::Polygon {
            exterior: bowtie.clone(),
            holes: vec![],
        };
        assert!((area(&g) - 500_000.0).abs() < 1.0);
        assert!(point_in_polygon([900.0, 500.0], &bowtie, &[]));
        assert!(point_in_polygon([100.0, 500.0], &bowtie, &[]));
        assert!(!point_in_polygon([500.0, 100.0], &bowtie, &[]));
        assert_eq!(polygon_validity(&bowtie, &[]), Validity::Invalid);
    }

    #[test]
    fn self_intersections_are_repaired_into_parts() {
        let bowtie = vec![[0.0, 0.0], [1000.0, 1000.0], [1000.0, 0.0], [0.0, 1000.0]];
        let n = normalize(
            Geometry::Polygon {
                exterior: bowtie,
                holes: vec![],
            },
            slide(),
        )
        .unwrap();
        assert!(n.repaired);
        assert_eq!(n.parts.len(), 2);
        let total: f64 = n.parts.iter().map(area).sum();
        assert!((total - 500_000.0).abs() < 1.0);
        for part in &n.parts {
            let Geometry::Polygon { exterior, holes } = part else {
                panic!()
            };
            assert_eq!(polygon_validity(exterior, holes), Validity::Valid);
        }
    }

    #[test]
    fn a_loop_inside_the_outline_is_detected() {
        // A ring that loops around a small area twice (winding number 2).
        let ring = vec![
            [0.0, 0.0],
            [100.0, 0.0],
            [100.0, 100.0],
            [40.0, 100.0],
            [40.0, 40.0],
            [60.0, 40.0],
            [60.0, 60.0],
            [30.0, 60.0],
            [30.0, 100.0],
            [0.0, 100.0],
        ];
        assert_eq!(polygon_validity(&ring, &[]), Validity::Invalid);
    }

    #[test]
    fn valid_polygons_keep_their_coordinates() {
        let ring = vec![[10.5, 10.25], [200.0, 10.0], [200.0, 300.0]];
        let n = normalize(
            Geometry::Polygon {
                exterior: ring.clone(),
                holes: vec![],
            },
            slide(),
        )
        .unwrap();
        assert!(!n.repaired && !n.clipped);
        assert_eq!(
            n.parts,
            vec![Geometry::Polygon {
                exterior: ring,
                holes: vec![]
            }]
        );
    }

    #[test]
    fn degenerate_shapes_are_rejected() {
        let collinear = Geometry::Polygon {
            exterior: vec![[0.0, 0.0], [5.0, 5.0], [10.0, 10.0]],
            holes: vec![],
        };
        assert!(normalize(collinear, slide()).is_err());
        let duplicates = Geometry::Polygon {
            exterior: vec![[0.0, 0.0], [0.0, 0.0], [1.0, 1.0], [1.0, 1.0]],
            holes: vec![],
        };
        assert!(normalize(duplicates, slide()).is_err());
        let nan = Geometry::Point {
            x: f64::NAN,
            y: 0.0,
        };
        assert!(normalize(nan, slide()).is_err());
        let line = Geometry::Polyline {
            points: vec![[1.0, 1.0], [1.0, 1.0]],
        };
        assert!(normalize(line, slide()).is_err());
        assert!(
            normalize(
                Geometry::Rectangle {
                    x: 0.0,
                    y: 0.0,
                    width: 0.0,
                    height: 5.0
                },
                slide()
            )
            .is_err()
        );
    }

    #[test]
    fn shapes_outside_the_slide_are_clipped_or_rejected() {
        assert!(normalize(Geometry::Point { x: -1.0, y: 5.0 }, slide()).is_err());
        let n = normalize(
            Geometry::Polygon {
                exterior: square(9_000.0, 9_000.0, 2_000.0),
                holes: vec![],
            },
            slide(),
        )
        .unwrap();
        assert!(n.clipped);
        assert!((area(&n.parts[0]) - 1_000_000.0).abs() < 1.0);
        let r = normalize(
            Geometry::Rectangle {
                x: -50.0,
                y: 10.0,
                width: 100.0,
                height: -20.0,
            },
            slide(),
        )
        .unwrap();
        assert_eq!(
            r.parts[0],
            Geometry::Rectangle {
                x: 0.0,
                y: 0.0,
                width: 50.0,
                height: 10.0
            }
        );
        assert!(
            normalize(
                Geometry::Polygon {
                    exterior: square(20_000.0, 0.0, 10.0),
                    holes: vec![]
                },
                slide()
            )
            .is_err()
        );
        let line = normalize(
            Geometry::Polyline {
                points: vec![[5.0, 5.0], [20_000.0, 5.0]],
            },
            slide(),
        )
        .unwrap();
        assert!(line.clipped);
    }

    #[test]
    fn boolean_operations_make_holes() {
        let outer = Geometry::Polygon {
            exterior: square(0.0, 0.0, 100.0),
            holes: vec![],
        };
        let cut = Geometry::Ellipse {
            cx: 50.0,
            cy: 50.0,
            rx: 10.0,
            ry: 10.0,
        };
        let result = difference(&outer, &cut).unwrap();
        assert_eq!(result.len(), 1);
        let Geometry::Polygon { holes, .. } = &result[0] else {
            panic!()
        };
        assert_eq!(holes.len(), 1);
        let expected = 10_000.0 - std::f64::consts::PI * 100.0;
        assert!((area(&result[0]) - expected).abs() < 0.1);
        // Cutting a band through the middle splits the square in two.
        let band = Geometry::Rectangle {
            x: -10.0,
            y: 45.0,
            width: 200.0,
            height: 10.0,
        };
        assert_eq!(difference(&outer, &band).unwrap().len(), 2);
        let merged = union(
            &outer,
            &Geometry::Rectangle {
                x: 90.0,
                y: 90.0,
                width: 20.0,
                height: 20.0,
            },
        )
        .unwrap();
        assert_eq!(merged.len(), 1);
        assert!((area(&merged[0]) - (10_000.0 + 400.0 - 100.0)).abs() < 1e-6);
    }

    #[test]
    fn regions_count_overlaps_once() {
        let a = Geometry::Rectangle {
            x: 0.0,
            y: 0.0,
            width: 10.0,
            height: 10.0,
        };
        let b = Geometry::Rectangle {
            x: 5.0,
            y: 0.0,
            width: 10.0,
            height: 10.0,
        };
        let region = Region::new(vec![a.clone(), b]);
        assert!((region.area_px() - 150.0).abs() < 1e-6);
        assert!(region.contains([12.0, 5.0]));
        assert!(!region.contains([20.0, 5.0]));
        let disjoint = Region::new(vec![
            a,
            Geometry::Ellipse {
                cx: 100.0,
                cy: 100.0,
                rx: 10.0,
                ry: 5.0,
            },
        ]);
        assert!((disjoint.area_px() - (100.0 + std::f64::consts::PI * 50.0)).abs() < 1e-9);
    }

    #[test]
    fn calipers_of_simple_shapes() {
        let (max, min) = calipers(&square(0.0, 0.0, 10.0));
        assert!((max - 200f64.sqrt()).abs() < 1e-9);
        assert!((min - 10.0).abs() < 1e-9);
        let rect = vec![[0.0, 0.0], [30.0, 0.0], [30.0, 10.0], [0.0, 10.0]];
        let (max, min) = calipers(&rect);
        assert!((max - 1000f64.sqrt()).abs() < 1e-9);
        assert!((min - 10.0).abs() < 1e-9);
        // A rotated rectangle: width stays 10.
        let rot: Vec<P> = rect
            .iter()
            .map(|p| {
                let (s, c) = 0.5f64.sin_cos();
                [p[0] * c - p[1] * s, p[0] * s + p[1] * c]
            })
            .collect();
        let (_, min) = calipers(&rot);
        assert!((min - 10.0).abs() < 1e-9);
        assert_eq!(calipers(&[[0.0, 0.0], [3.0, 4.0]]), (5.0, 0.0));
    }

    #[test]
    fn signed_area_is_precise_far_from_the_origin() {
        let ring = square(150_000.0, 90_000.0, 1.0);
        assert!((ring_area(&ring) - 1.0).abs() < 1e-9);
    }
}
