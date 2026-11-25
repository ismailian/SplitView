//! The scratchpad's document: text notes, pen strokes and shapes on a canvas,
//! one document per project directory (the nearest ancestor with a `.git`,
//! else the directory itself). Coordinates are canvas points.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Shape {
    Pen,
    Line,
    Arrow,
    Rect,
    Ellipse,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum Item {
    /// A note; `at` is its top-left corner.
    Text {
        at: [f32; 2],
        text: String,
        color: u8,
    },
    /// `Pen`: every sampled point; the other shapes: `[start, end]`.
    Ink {
        shape: Shape,
        points: Vec<[f32; 2]>,
        color: u8,
    },
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Doc {
    /// The project directory, for humans reading the file.
    pub dir: String,
    pub items: Vec<Item>,
}

fn sub(a: [f32; 2], b: [f32; 2]) -> [f32; 2] {
    [a[0] - b[0], a[1] - b[1]]
}

/// Distance from `p` to the segment `a`–`b`.
pub fn seg_dist(p: [f32; 2], a: [f32; 2], b: [f32; 2]) -> f32 {
    let (ab, ap) = (sub(b, a), sub(p, a));
    let len2 = ab[0] * ab[0] + ab[1] * ab[1];
    let t = if len2 > 0.0 {
        ((ap[0] * ab[0] + ap[1] * ab[1]) / len2).clamp(0.0, 1.0)
    } else {
        0.0
    };
    let d = [ap[0] - t * ab[0], ap[1] - t * ab[1]];
    (d[0] * d[0] + d[1] * d[1]).sqrt()
}

/// Is `p` within `tol` of the ink's outline?
pub fn hit_ink(shape: Shape, points: &[[f32; 2]], p: [f32; 2], tol: f32) -> bool {
    match (shape, points) {
        (_, []) => false,
        (_, [a]) => seg_dist(p, *a, *a) <= tol,
        (Shape::Pen, pts) => pts.windows(2).any(|w| seg_dist(p, w[0], w[1]) <= tol),
        (Shape::Line | Shape::Arrow, [a, b, ..]) => seg_dist(p, *a, *b) <= tol,
        (Shape::Rect, [a, b, ..]) => {
            let (c, d) = ([a[0], b[1]], [b[0], a[1]]);
            [(*a, c), (c, *b), (*b, d), (d, *a)]
                .iter()
                .any(|&(x, y)| seg_dist(p, x, y) <= tol)
        }
        (Shape::Ellipse, [a, b, ..]) => {
            let c = [(a[0] + b[0]) / 2.0, (a[1] + b[1]) / 2.0];
            let (rx, ry) = (((b[0] - a[0]) / 2.0).abs(), ((b[1] - a[1]) / 2.0).abs());
            if rx < 1.0 || ry < 1.0 {
                return seg_dist(p, *a, *b) <= tol;
            }
            let (dx, dy) = ((p[0] - c[0]) / rx, (p[1] - c[1]) / ry);
            // Radial error scaled back to points: close enough for an eraser.
            ((dx * dx + dy * dy).sqrt() - 1.0).abs() * rx.min(ry) <= tol
        }
    }
}

/// Move an item by `d`.
pub fn translate(item: &mut Item, d: [f32; 2]) {
    match item {
        Item::Text { at, .. } => *at = [at[0] + d[0], at[1] + d[1]],
        Item::Ink { points, .. } => {
            for p in points {
                *p = [p[0] + d[0], p[1] + d[1]];
            }
        }
    }
}

/// Bounding box (min, max) of some ink.
pub fn bounds(points: &[[f32; 2]]) -> Option<([f32; 2], [f32; 2])> {
    let first = *points.first()?;
    Some(points.iter().fold((first, first), |(lo, hi), p| {
        (
            [lo[0].min(p[0]), lo[1].min(p[1])],
            [hi[0].max(p[0]), hi[1].max(p[1])],
        )
    }))
}

/// The project a directory belongs to: the nearest ancestor (or itself) for
/// which `is_repo` is true, else the directory itself.
pub fn project_root(dir: &str, is_repo: impl Fn(&str) -> bool) -> String {
    std::path::Path::new(dir)
        .ancestors()
        .map(|p| p.to_string_lossy().into_owned())
        .find(|p| !p.is_empty() && is_repo(p))
        .unwrap_or_else(|| dir.to_owned())
}

/// The file a project's pad is stored in: a stable FNV-1a hash of the
/// normalized path (case-folded for Windows paths), `global.json` for none.
pub fn file_name(dir: &str) -> String {
    let d = dir.trim_end_matches(['/', '\\']);
    if d.is_empty() {
        return "global.json".into();
    }
    let norm = if crate::paths::is_windows_path(d) {
        d.replace('/', "\\").to_lowercase()
    } else {
        d.to_owned()
    };
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in norm.bytes() {
        h ^= b as u64;
        h = h.wrapping_mul(0x0100_0000_01b3);
    }
    format!("{h:016x}.json")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hits_outlines_not_interiors() {
        let r = [[0.0, 0.0], [100.0, 50.0]];
        assert!(hit_ink(Shape::Rect, &r, [50.0, 1.0], 3.0));
        assert!(!hit_ink(Shape::Rect, &r, [50.0, 25.0], 3.0));
        assert!(hit_ink(Shape::Ellipse, &r, [50.0, 0.5], 3.0));
        assert!(!hit_ink(Shape::Ellipse, &r, [50.0, 25.0], 3.0));
        assert!(hit_ink(Shape::Line, &r, [50.0, 25.0], 1.0));
        let pen = [[0.0, 0.0], [10.0, 0.0], [10.0, 10.0]];
        assert!(hit_ink(Shape::Pen, &pen, [11.0, 5.0], 2.0));
        assert!(!hit_ink(Shape::Pen, &pen, [5.0, 5.0], 2.0));
        assert!(!hit_ink(Shape::Pen, &[], [0.0, 0.0], 9.0));
    }

    #[test]
    fn moves_and_bounds() {
        let mut ink = Item::Ink {
            shape: Shape::Pen,
            points: vec![[0.0, 5.0], [10.0, -2.0]],
            color: 0,
        };
        translate(&mut ink, [1.0, 1.0]);
        let Item::Ink { points, .. } = &ink else {
            unreachable!()
        };
        assert_eq!(bounds(points), Some(([1.0, -1.0], [11.0, 6.0])));
        assert_eq!(bounds(&[]), None);
        let mut note = Item::Text {
            at: [3.0, 4.0],
            text: "x".into(),
            color: 0,
        };
        translate(&mut note, [-3.0, 1.0]);
        assert!(matches!(note, Item::Text { at: [0.0, 5.0], .. }));
    }

    #[test]
    fn project_is_the_repo_root() {
        let repo = |p: &str| p == "/home/u/proj";
        assert_eq!(project_root("/home/u/proj/src/a", repo), "/home/u/proj");
        assert_eq!(project_root("/tmp/x", repo), "/tmp/x");
    }

    #[test]
    fn file_names_are_stable_and_normalized() {
        assert_eq!(file_name(""), "global.json");
        assert_eq!(file_name(r"D:\Proj\"), file_name("d:/proj"));
        assert_ne!(file_name("/a/b"), file_name("/a/B"));
        // Pinned: a changed hash would orphan every saved pad.
        assert_eq!(file_name("/a"), "07d66707b49cd92d.json");
    }

    #[test]
    fn doc_round_trips_and_tolerates_missing_fields() {
        let d = Doc {
            dir: "/p".into(),
            items: vec![
                Item::Text {
                    at: [1.0, 2.0],
                    text: "hi".into(),
                    color: 0,
                },
                Item::Ink {
                    shape: Shape::Arrow,
                    points: vec![[0.0, 0.0], [5.0, 5.0]],
                    color: 2,
                },
            ],
        };
        let s = serde_json::to_string(&d).unwrap();
        assert_eq!(serde_json::from_str::<Doc>(&s).unwrap(), d);
        assert_eq!(serde_json::from_str::<Doc>("{}").unwrap(), Doc::default());
    }
}
