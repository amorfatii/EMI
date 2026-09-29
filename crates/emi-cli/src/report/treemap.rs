//! The codebase map: a squarified treemap (Bruls, Huizing and van Wijk,
//! 2000) of the measured files, grouped by top-level folder. Area is lines
//! of code, so sizes compare at a glance; fill is how often the file
//! changed in the history window, in five ordered classes of one hue.

use std::fmt::Write as _;

use super::page::{count, esc, interval};

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Rect {
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
}

/// Rectangles with areas proportional to `values`, as square as the
/// values allow, in the input order. A value that is not positive gets an
/// empty rectangle.
pub fn squarify(values: &[f64], r: Rect) -> Vec<Rect> {
    let mut out = vec![
        Rect {
            x: r.x,
            y: r.y,
            w: 0.0,
            h: 0.0,
        };
        values.len()
    ];
    let total: f64 = values.iter().filter(|v| **v > 0.0).sum();
    if total <= 0.0 || r.w <= 0.0 || r.h <= 0.0 {
        return out;
    }
    let mut order: Vec<usize> = (0..values.len()).filter(|i| values[*i] > 0.0).collect();
    order.sort_by(|a, b| values[*b].total_cmp(&values[*a]));
    let scale = r.w * r.h / total;
    let areas: Vec<(usize, f64)> = order.iter().map(|i| (*i, values[*i] * scale)).collect();
    let (mut free, mut start) = (r, 0);
    while start < areas.len() {
        let side = free.w.min(free.h);
        let mut end = start + 1;
        while end < areas.len()
            && worst(&areas[start..=end], side) <= worst(&areas[start..end], side)
        {
            end += 1;
        }
        free = place(&areas[start..end], free, &mut out);
        start = end;
    }
    out
}

/// The worst aspect ratio of a row laid along a side of length `side`.
fn worst(row: &[(usize, f64)], side: f64) -> f64 {
    let sum: f64 = row.iter().map(|r| r.1).sum();
    let max = row.iter().map(|r| r.1).fold(0.0, f64::max);
    let min = row.iter().map(|r| r.1).fold(f64::INFINITY, f64::min);
    let (s2, w2) = (sum * sum, side * side);
    (w2 * max / s2).max(s2 / (w2 * min))
}

/// Place a row along the shorter side of `free`; the rest is returned.
fn place(row: &[(usize, f64)], free: Rect, out: &mut [Rect]) -> Rect {
    let sum: f64 = row.iter().map(|r| r.1).sum();
    if free.w >= free.h {
        let w = sum / free.h;
        let mut y = free.y;
        for (i, a) in row {
            let h = a / w;
            out[*i] = Rect { x: free.x, y, w, h };
            y += h;
        }
        Rect {
            x: free.x + w,
            w: free.w - w,
            ..free
        }
    } else {
        let h = sum / free.w;
        let mut x = free.x;
        for (i, a) in row {
            let w = a / h;
            out[*i] = Rect { x, y: free.y, w, h };
            x += w;
        }
        Rect {
            y: free.y + h,
            h: free.h - h,
            ..free
        }
    }
}

/// One file on the map.
pub struct Tile {
    pub path: String,
    pub sloc: f64,
    pub revisions: u32,
    pub burden: [f64; 2],
    /// Rank among the hotspots (1-based), for the top few.
    pub hotspot: Option<usize>,
}

/// Lower bounds of the five change classes: about evenly spaced on a log
/// scale up to the most changed file, whole numbers, strictly increasing.
pub fn class_edges(max_revisions: u32) -> Vec<u32> {
    let max = f64::from(max_revisions.max(1));
    let mut edges = vec![1];
    for k in 1..5 {
        let e = max.powf(f64::from(k) / 5.0).round() as u32;
        let last = *edges.last().unwrap_or(&1);
        if e > last && e <= max_revisions {
            edges.push(e);
        }
    }
    edges
}

/// The class of a file (0: not changed in the window), and its range text.
pub fn class_of(revisions: u32, edges: &[u32]) -> usize {
    if revisions == 0 {
        return 0;
    }
    edges
        .iter()
        .rposition(|e| revisions >= *e)
        .map_or(1, |i| i + 1)
}

/// Legend text for each class: `1`, `2–3`, ..., `10–18`.
pub fn class_labels(edges: &[u32], max: u32) -> Vec<String> {
    edges
        .iter()
        .enumerate()
        .map(|(i, lo)| {
            let hi = edges.get(i + 1).map_or(max, |next| next - 1);
            if hi <= *lo {
                lo.to_string()
            } else {
                format!("{lo}–{hi}")
            }
        })
        .collect()
}

/// Drawing sizes: landscape, and the portrait map narrow screens show
/// instead (SVG text scales with the drawing, so no one drawing stays
/// legible at both widths).
pub const WIDE: (f64, f64) = (1000.0, 560.0);
pub const NARROW: (f64, f64) = (360.0, 480.0);
/// Approximate width of one 11px character, for fit checks.
const CHAR_W: f64 = 6.4;

fn group_of(path: &str) -> &str {
    path.split_once('/').map_or("", |(g, _)| g)
}

fn basename(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

/// Group the tiles by top-level folder: `(name, indices)`, largest first.
fn groups(tiles: &[Tile]) -> Vec<(String, Vec<usize>)> {
    let mut map: std::collections::BTreeMap<&str, Vec<usize>> = std::collections::BTreeMap::new();
    for (i, t) in tiles.iter().enumerate() {
        map.entry(group_of(&t.path)).or_default().push(i);
    }
    let mut out: Vec<(String, Vec<usize>)> =
        map.into_iter().map(|(g, ix)| (g.to_owned(), ix)).collect();
    let size = |ix: &[usize]| ix.iter().map(|i| tiles[*i].sloc).sum::<f64>();
    out.sort_by(|a, b| size(&b.1).total_cmp(&size(&a.1)));
    out
}

/// The label of a folder band that fits `width`: name and size, else the
/// name alone, else the name shortened.
fn fitted(name: &str, lines: f64, width: f64) -> Option<String> {
    let room = ((width - 12.0) / CHAR_W).floor() as usize;
    let full = format!("{name} · {}", count(lines, "line", "lines"));
    if room >= full.chars().count() {
        Some(full)
    } else if room >= name.chars().count() {
        Some(name.to_owned())
    } else if room >= 6 {
        Some(name.chars().take(room - 1).collect::<String>() + "…")
    } else {
        None
    }
}

fn tip(t: &Tile) -> String {
    let mut s = format!(
        "{}\n{} · {} · burden {}",
        t.path,
        count(t.sloc, "line", "lines"),
        count(f64::from(t.revisions), "change", "changes"),
        interval(t.burden)
    );
    if let Some(n) = t.hotspot {
        let _ = write!(s, "\nhotspot #{n}");
    }
    s
}

fn file_svg(t: &Tile, r: Rect, class: usize, svg: &mut String) {
    let (x, y, w, h) = (
        r.x + 1.0,
        r.y + 1.0,
        (r.w - 2.0).max(0.0),
        (r.h - 2.0).max(0.0),
    );
    if w <= 0.0 || h <= 0.0 {
        return;
    }
    let tip = esc(&tip(t));
    let _ = write!(
        svg,
        r#"<g class="cell b{class}" tabindex="0" role="img" data-tip="{tip}" aria-label="{tip}"><rect x="{x:.1}" y="{y:.1}" width="{w:.1}" height="{h:.1}" rx="2"/>"#
    );
    let badge = t.hotspot.filter(|_| w >= 24.0 && h >= 24.0);
    if let Some(n) = badge {
        let _ = write!(
            svg,
            r#"<circle class="badge" cx="{:.1}" cy="{:.1}" r="8"/><text class="badge-n" x="{:.1}" y="{:.1}">{n}</text>"#,
            x + 12.0,
            y + 12.0,
            x + 12.0,
            y + 15.5
        );
    }
    labels(t, Rect { x, y, w, h }, badge.is_some(), svg);
    svg.push_str("</g>");
}

/// The file's name and size, one per 15px line, each only where it fits;
/// the name goes below a hotspot badge that leaves it no room beside.
fn labels(t: &Tile, r: Rect, badge: bool, svg: &mut String) {
    let fits =
        |text: &str, from: f64| from + text.chars().count() as f64 * CHAR_W + 6.0 <= r.x + r.w;
    let name = basename(&t.path);
    let (name_x, line) = match (badge, fits(name, r.x + 24.0)) {
        (true, true) => (r.x + 24.0, 0.0),
        (true, false) => (r.x + 6.0, 1.0),
        (false, _) => (r.x + 6.0, 0.0),
    };
    let baseline = |k: f64| r.y + 15.5 + 15.0 * k;
    if baseline(line) + 5.0 > r.y + r.h || !fits(name, name_x) {
        return;
    }
    let _ = write!(
        svg,
        r#"<text class="on" x="{name_x:.1}" y="{:.1}">{}</text>"#,
        baseline(line),
        esc(name)
    );
    let size = count(t.sloc, "line", "lines");
    if baseline(line + 1.0) + 5.0 <= r.y + r.h && fits(&size, r.x + 6.0) {
        let _ = write!(
            svg,
            r#"<text class="on sub" x="{:.1}" y="{:.1}">{size}</text>"#,
            r.x + 6.0,
            baseline(line + 1.0)
        );
    }
}

/// The folder's rectangle: a label band when there is room, files below.
fn group_svg(name: &str, ix: &[usize], r: Rect, tiles: &[Tile], edges: &[u32], svg: &mut String) {
    let inner = Rect {
        x: r.x + 3.0,
        y: r.y + 3.0,
        w: (r.w - 6.0).max(0.0),
        h: (r.h - 6.0).max(0.0),
    };
    let lines: f64 = ix.iter().map(|i| tiles[*i].sloc).sum();
    let label = if name.is_empty() { "(root)" } else { name };
    let band = inner.h >= 64.0 && inner.w >= 90.0;
    let body = if band {
        if let Some(text) = fitted(label, lines, inner.w) {
            let _ = write!(
                svg,
                r#"<text class="group" x="{:.1}" y="{:.1}">{}</text>"#,
                inner.x + 2.0,
                inner.y + 12.0,
                esc(&text)
            );
        }
        Rect {
            y: inner.y + 18.0,
            h: inner.h - 18.0,
            ..inner
        }
    } else {
        inner
    };
    let values: Vec<f64> = ix.iter().map(|i| tiles[*i].sloc.max(1.0)).collect();
    for (k, rect) in squarify(&values, body).into_iter().enumerate() {
        let t = &tiles[ix[k]];
        file_svg(t, rect, class_of(t.revisions, edges), svg);
    }
}

fn max_revisions(tiles: &[Tile]) -> u32 {
    tiles.iter().map(|t| t.revisions).max().unwrap_or(0)
}

/// The legend text of each change class the map uses (none when no file
/// changed in the window).
pub fn legend_labels(tiles: &[Tile]) -> Vec<String> {
    match max_revisions(tiles) {
        0 => Vec::new(),
        max => class_labels(&class_edges(max), max),
    }
}

/// The map as an SVG element of `(width, height)`, with CSS class `class`.
pub fn render(tiles: &[Tile], (w, h): (f64, f64), class: &str) -> String {
    let edges = class_edges(max_revisions(tiles));
    let mut svg = format!(
        r#"<svg class="{class}" viewBox="0 0 {w} {h}" role="group" aria-label="Map of {} files: area is lines of code, color is how often each file changed">"#,
        tiles.len()
    );
    let groups = groups(tiles);
    let sizes: Vec<f64> = groups
        .iter()
        .map(|(_, ix)| ix.iter().map(|i| tiles[*i].sloc.max(1.0)).sum())
        .collect();
    let canvas = Rect {
        x: 0.0,
        y: 0.0,
        w,
        h,
    };
    for ((name, ix), r) in groups.iter().zip(squarify(&sizes, canvas)) {
        group_svg(name, ix, r, tiles, &edges, &mut svg);
    }
    svg + "</svg>"
}

#[cfg(test)]
mod tests {
    use super::*;

    const CANVAS: Rect = Rect {
        x: 0.0,
        y: 0.0,
        w: 300.0,
        h: 200.0,
    };

    #[test]
    fn areas_are_proportional_and_inside_the_canvas() {
        let values = [6.0, 6.0, 4.0, 3.0, 2.0, 2.0, 1.0, 0.0];
        let rects = squarify(&values, CANVAS);
        let total: f64 = values.iter().sum();
        for (v, r) in values.iter().zip(&rects) {
            let expected = v / total * CANVAS.w * CANVAS.h;
            assert!((r.w * r.h - expected).abs() < 1e-6, "{v}: {r:?}");
            assert!(r.x >= -1e-9 && r.y >= -1e-9, "{r:?}");
            assert!(
                r.x + r.w <= CANVAS.w + 1e-6 && r.y + r.h <= CANVAS.h + 1e-6,
                "{r:?}"
            );
        }
        // squarified: no sliver when the values allow squares
        let ratio = |r: &Rect| (r.w / r.h).max(r.h / r.w);
        assert!(rects[..7].iter().all(|r| ratio(r) < 3.0), "{rects:?}");
    }

    #[test]
    fn change_classes_cover_one_to_the_maximum() {
        let edges = class_edges(18);
        assert_eq!(edges, vec![1, 2, 3, 6, 10]);
        assert_eq!(
            class_labels(&edges, 18),
            vec!["1", "2", "3–5", "6–9", "10–18"]
        );
        assert_eq!(class_of(0, &edges), 0);
        assert_eq!(class_of(1, &edges), 1);
        assert_eq!(class_of(4, &edges), 3);
        assert_eq!(class_of(18, &edges), 5);
        // few distinct counts: fewer classes, never a repeated edge
        assert_eq!(class_edges(2), vec![1, 2]);
        assert_eq!(class_edges(1), vec![1]);
    }

    #[test]
    fn labels_appear_only_where_they_fit() {
        let tiles = vec![
            Tile {
                path: "a/big_module_name.py".into(),
                sloc: 900.0,
                revisions: 9,
                burden: [10.0, 10.0],
                hotspot: Some(1),
            },
            // drawable (about 32px wide) but too narrow for its name
            Tile {
                path: "a/tiny.py".into(),
                sloc: 30.0,
                revisions: 0,
                burden: [0.0, 0.0],
                hotspot: None,
            },
        ];
        let svg = render(&tiles, WIDE, "map");
        assert!(svg.contains(">big_module_name.py<"), "{svg}");
        assert!(!svg.contains(">tiny.py<"), "a sliver gets no label");
        assert!(
            svg.contains("data-tip=\"a/tiny.py"),
            "but keeps its tooltip"
        );
        assert!(svg.contains("class=\"badge-n\""));
        assert_eq!(legend_labels(&tiles).len(), class_edges(9).len());
        let unchanged: Vec<Tile> = tiles
            .into_iter()
            .map(|t| Tile {
                revisions: 0,
                hotspot: None,
                ..t
            })
            .collect();
        assert!(
            legend_labels(&unchanged).is_empty(),
            "no change, no classes"
        );
    }
}
