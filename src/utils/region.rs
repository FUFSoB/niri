use std::cmp::{max, min};
use std::collections::BTreeSet;
use std::sync::Arc;

use smithay::utils::{Logical, Physical, Point, Rectangle, Scale, Size, Transform};
use smithay::wayland::compositor::{RectangleKind, RegionAttributes};

/// Helper for fractionally transforming an i32 region while preserving adjacent rects.
///
/// Naively applying floating point transforms may cause adjacent rects to go misaligned due to
/// rounding differences. This struct helps apply the transforms in such a way as to preserve
/// alignment.
#[derive(Debug, Clone)]
pub struct TransformedRegion {
    /// Non-overlapping rects (usually in surface-local coordinates).
    pub rects: Arc<Vec<Rectangle<i32, Logical>>>,
    /// Scale to apply to each rect.
    pub scale: Scale<f64>,
    /// Translation to apply to each rect after scaling, before any transform.
    pub offset: Point<f64, Logical>,
    /// Translation to apply after the transform.
    pub post_transform_offset: Point<f64, Logical>,
    /// Additional compositor transform to apply after scaling and offsetting.
    pub transform: Transform,
    /// Area to transform inside, in the same coordinate space as the scaled+offset rects.
    pub transform_area: Size<f64, Logical>,
}

impl TransformedRegion {
    /// Returns an iterator over the top-left and bottom-right corners of transformed rects.
    pub fn iter(&self) -> impl Iterator<Item = (Point<f64, Logical>, Point<f64, Logical>)> + '_ {
        self.rects.iter().map(|r| {
            let mut r = r.to_f64();
            r.loc = r.loc.upscale(self.scale) + self.offset;
            r.size = r.size.upscale(self.scale);

            if self.transform != Transform::Normal {
                r = self.transform.transform_rect_in(r, &self.transform_area);
            }

            r.loc += self.post_transform_offset;

            (r.loc, r.loc + r.size.to_point())
        })
    }

    /// Intersects damage with this subregion.
    pub fn filter_damage(
        &self,
        // Same coordinate space as self.iter().
        crop: Rectangle<f64, Logical>,
        dst: Rectangle<i32, Physical>,
        damage: &[Rectangle<i32, Physical>],
        filtered: &mut Vec<Rectangle<i32, Physical>>,
    ) {
        let scale = dst.size.to_f64() / crop.size;

        let cs = crop.size.to_point();

        for (mut a, mut b) in self.iter() {
            // Convert to dst-relative.
            a -= crop.loc;
            b -= crop.loc;

            // Intersect with crop.
            let ia = Point::new(f64::max(a.x, 0.), f64::max(a.y, 0.));
            let ib = Point::new(f64::min(b.x, cs.x), f64::min(b.y, cs.y));
            if ib.x <= ia.x || ib.y <= ia.y {
                // No intersection.
                continue;
            }

            // Round extremities to physical pixels, ensuring that adjacent rectangles stay adjacent
            // at fractional scales.
            let ia = ia.to_physical_precise_round(scale);
            let ib = ib.to_physical_precise_round(scale);

            let r = Rectangle::from_extremities(ia, ib);

            // Intersect with each damage rect.
            for d in damage {
                if let Some(intersection) = r.intersection(*d) {
                    filtered.push(intersection);
                }
            }
        }
    }
}

pub fn region_to_non_overlapping_rects(
    region: &RegionAttributes,
    output: &mut Vec<Rectangle<i32, Logical>>,
) {
    let _span = tracy_client::span!("region_to_non_overlapping_rects");

    output.clear();

    // Collect all unique Y coordinates.
    let ys = BTreeSet::from_iter(
        region
            .rects
            .iter()
            .flat_map(|(_, r)| [r.loc.y, r.loc.y + r.size.h]),
    );

    let mut ys = ys.into_iter();
    let Some(mut lo) = ys.next() else {
        // The region was empty.
        return;
    };

    // Sorted list of non-overlapping [start, end) tuples.
    let mut spans = Vec::<(i32, i32)>::new();

    // Iterate over Y bands.
    for hi in ys {
        spans.clear();

        'region: for (kind, r) in &region.rects {
            // Skip rects that don't overlap with the Y band.
            if hi <= r.loc.y || r.loc.y + r.size.h <= lo {
                continue;
            }

            let mut x1 = r.loc.x;
            let mut x2 = r.loc.x + r.size.w;
            if x1 == x2 {
                // Empty rect.
                continue;
            }

            match *kind {
                RectangleKind::Add => {
                    // Iterate over existing spans backwards.
                    for i in (0..spans.len()).rev() {
                        let (start, end) = spans[i];

                        // New span is to the right.
                        if end < x1 {
                            spans.insert(i + 1, (x1, x2));
                            continue 'region;
                        }

                        // New span is to the left.
                        if x2 < start {
                            continue;
                        }

                        // New span overlaps this span; merge them.
                        spans.remove(i);
                        x1 = min(x1, start);
                        x2 = max(x2, end);
                    }

                    spans.insert(0, (x1, x2));
                }
                RectangleKind::Subtract => {
                    // Iterate over existing spans backwards.
                    for i in (0..spans.len()).rev() {
                        let (start, end) = spans[i];

                        // Subtract span is to the right.
                        if end <= x1 {
                            continue 'region;
                        }

                        // Subtract span is to the left.
                        if x2 <= start {
                            continue;
                        }

                        // Subtract span overlaps this span.
                        spans.remove(i);
                        if x2 < end {
                            spans.insert(i, (x2, end));
                        }
                        if start < x1 {
                            spans.insert(i, (start, x1));
                        }
                    }
                }
            }
        }

        for (x1, x2) in spans.drain(..) {
            output.push(Rectangle::from_extremities((x1, lo), (x2, hi)));
        }

        lo = hi;
    }
}

#[cfg(test)]
mod tests {
    use std::fmt::Write as _;
    use std::sync::Arc;

    use insta::assert_snapshot;
    use proptest::prelude::*;
    use smithay::utils::{Logical, Point, Rectangle, Scale, Size, Transform};
    use smithay::wayland::compositor::{RectangleKind, RegionAttributes};

    use super::{region_to_non_overlapping_rects, TransformedRegion};

    #[allow(clippy::type_complexity)]
    fn check(rects: &[(RectangleKind, (i32, i32, i32, i32))]) -> String {
        let region = RegionAttributes {
            rects: rects
                .iter()
                .map(|(kind, (x1, y1, x2, y2))| {
                    (*kind, Rectangle::from_extremities((*x1, *y1), (*x2, *y2)))
                })
                .collect(),
        };
        let mut output = Vec::new();
        region_to_non_overlapping_rects(&region, &mut output);
        let mut s = String::new();
        for r in &output {
            let x1 = r.loc.x;
            let y1 = r.loc.y;
            let x2 = x1 + r.size.w;
            let y2 = y1 + r.size.h;
            writeln!(s, "{x1:2} {y1:2} - {x2:2} {y2:2}").unwrap();
        }
        s
    }

    #[test]
    fn test_region_to_non_overlapping_rects() {
        use RectangleKind::*;

        // empty_region
        assert_snapshot!(check(&[]), @"");

        // single_rectangle
        assert_snapshot!(check(&[(Add, (0, 0, 10, 10))]), @" 0  0 - 10 10");

        // empty_rectangle
        assert_snapshot!(check(&[(Add, (0, 0, 0, 1))]), @"");
        assert_snapshot!(check(&[(Add, (0, 0, 1, 0))]), @"");

        // two_non_overlapping
        assert_snapshot!(
            check(&[(Add, (0, 0, 5, 10)), (Add, (7, 0, 12, 10))]),
            @"
        0  0 -  5 10
        7  0 - 12 10
        "
        );

        // two_overlapping
        assert_snapshot!(
            check(&[(Add, (0, 0, 10, 10)), (Add, (5, 5, 15, 15))]),
            @"
        0  0 - 10  5
        0  5 - 15 10
        5 10 - 15 15
        "
        );

        // subtraction
        assert_snapshot!(
            check(&[(Add, (0, 0, 20, 20)), (Subtract, (5, 5, 15, 15))]),
            @"
         0  0 - 20  5
         0  5 -  5 15
        15  5 - 20 15
         0 15 - 20 20
        "
        );

        // adjacent_rectangles
        assert_snapshot!(
            check(&[(Add, (0, 0, 10, 10)), (Add, (10, 0, 20, 10))]),
            @" 0  0 - 20 10"
        );
    }

    #[test]
    fn transformed_region_applies_post_transform_offset_last() {
        let region = TransformedRegion {
            rects: Arc::new(vec![Rectangle::new(
                Point::from((0, 0)),
                Size::from((4, 2)),
            )]),
            scale: Scale::from(1.),
            offset: Point::from((10., 0.)),
            post_transform_offset: Point::from((100., 50.)),
            transform: Transform::_180,
            transform_area: Size::from((20., 10.)),
        };

        let rects = region.iter().collect::<Vec<_>>();

        assert_eq!(
            rects,
            vec![(Point::from((106., 58.)), Point::from((110., 60.)))]
        );
    }

    proptest! {
        #[test]
        fn non_overlapping_output(
            rects in proptest::collection::vec(
                (
                    prop_oneof![Just(RectangleKind::Add), Just(RectangleKind::Subtract)],
                    (0..20i32, 0..20i32, 0..20i32, 0..20i32),
                ),
                1..10,
            )
        ) {
            let region = RegionAttributes {
                rects: rects
                    .into_iter()
                    .map(|(kind, (x, y, w, h))| {
                        (kind, Rectangle::new(Point::new(x, y), Size::new(w, h)))
                    })
                    .collect(),
            };

            let mut output: Vec<Rectangle<i32, Logical>> = Vec::new();
            region_to_non_overlapping_rects(&region, &mut output);

            for i in 0..output.len() {
                prop_assert!(!output[i].is_empty());

                // Verify no pair of output rectangles overlaps.
                for j in (i + 1)..output.len() {
                    prop_assert!(
                        !output[i].overlaps(output[j]),
                        "rectangles overlap: {:?} and {:?}",
                        output[i],
                        output[j],
                    );
                }
            }
        }
    }
}
