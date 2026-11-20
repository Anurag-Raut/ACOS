//! `android.graphics.Region` — a rectangle-list with boolean ops.
//!
//! Android's Region is an integer scanline region. We implement it over a
//! temporary bitset for boolean operations (correct at pixel granularity) and
//! compress back to a sorted, non-overlapping rectangle list. This matches the
//! observable `Region` API (`op`, `contains`, `getBounds`, `quickReject`,
//! `setPath`, `translate`, `scale`).

use crate::geom::{Rect, RectF};
use crate::path::{FillType, Path};
use crate::RenderError;
use tiny_skia::Mask;

/// Android `Region.Op` ordinals.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum RegionOp {
    Difference = 0,
    Intersect = 1,
    Union = 2,
    Xor = 3,
    ReverseDifference = 4,
    Replace = 5,
}

impl RegionOp {
    pub fn from_ordinal(o: i32) -> Option<Self> {
        Some(match o {
            0 => RegionOp::Difference,
            1 => RegionOp::Intersect,
            2 => RegionOp::Union,
            3 => RegionOp::Xor,
            4 => RegionOp::ReverseDifference,
            5 => RegionOp::Replace,
            _ => return None,
        })
    }
}

const MAX_AREA: i64 = 64_000_000;

/// An integer region.
#[derive(Clone, PartialEq, Debug, Default)]
pub struct Region {
    rects: Vec<Rect>,
    bounds: Option<Rect>,
}

impl Region {
    pub fn new() -> Self {
        Region {
            rects: Vec::new(),
            bounds: None,
        }
    }

    pub fn from_rect(r: Rect) -> Self {
        let mut region = Region::new();
        region.set_rect(r);
        region
    }

    pub fn is_empty(&self) -> bool {
        self.rects.is_empty()
    }

    pub fn bounds(&self) -> Option<Rect> {
        self.bounds
    }

    pub fn rects(&self) -> &[Rect] {
        &self.rects
    }

    /// Recompute the cached bounds and normalise the rect list.
    fn set_rects(&mut self, mut rects: Vec<Rect>) {
        rects.retain(|r| !r.is_empty());
        if rects.is_empty() {
            self.rects.clear();
            self.bounds = None;
            return;
        }
        let mut min_x = i32::MAX;
        let mut min_y = i32::MAX;
        let mut max_x = i32::MIN;
        let mut max_y = i32::MIN;
        for r in &rects {
            min_x = min_x.min(r.left);
            min_y = min_y.min(r.top);
            max_x = max_x.max(r.right);
            max_y = max_y.max(r.bottom);
        }
        rects.sort_by_key(|r| (r.top, r.left, r.bottom, r.right));
        self.rects = rects;
        self.bounds = Some(Rect::new(min_x, min_y, max_x, max_y));
    }

    pub fn set_rect(&mut self, r: Rect) {
        self.set_rects(if r.is_empty() { vec![] } else { vec![r] });
    }

    pub fn set_region(&mut self, other: &Region) {
        self.rects = other.rects.clone();
        self.bounds = other.bounds;
    }

    /// `Region.setPath`.
    pub fn set_path(&mut self, path: &Path, clip: Option<&Region>) -> Result<(), RenderError> {
        let Some(b) = path.compute_bounds() else {
            self.set_rects(vec![]);
            return Ok(());
        };
        let bb = b.round_out();
        let Some(clipb) = clip.and_then(|c| c.bounds) else {
            return self.set_path_in(path, bb, path.fill_type());
        };
        let area = bb.intersect(&Rect::new(
            clipb.left,
            clipb.top,
            clipb.right.max(clipb.left + 1),
            clipb.bottom.max(clipb.top + 1),
        ));
        self.set_path_in(path, area, path.fill_type())
    }

    fn set_path_in(
        &mut self,
        path: &Path,
        bb: Rect,
        fill_type: FillType,
    ) -> Result<(), RenderError> {
        if bb.is_empty() {
            self.set_rects(vec![]);
            return Ok(());
        }
        let w = bb.width() as u32;
        let h = bb.height() as u32;
        let area = (w as i64) * (h as i64);
        if area > MAX_AREA {
            return Err(RenderError::size(format!(
                "Region.setPath bounds {w}x{h} exceeds {MAX_AREA} pixels"
            )));
        }
        let Some(skpath) = path.build_tiny_skia() else {
            self.set_rects(vec![]);
            return Ok(());
        };
        let Some(mut mask) = Mask::new(w, h) else {
            self.set_rects(vec![]);
            return Ok(());
        };
        // Translate the path into mask-local coordinates.
        let local = skpath
            .clone()
            .transform(tiny_skia::Transform::from_translate(
                -(bb.left as f32),
                -(bb.top as f32),
            ))
            .unwrap_or(skpath);
        mask.fill_path(
            &local,
            fill_type.fill_rule(),
            false,
            tiny_skia::Transform::identity(),
        );
        if fill_type.is_inverse() {
            mask.invert();
        }
        let bits = mask_to_bitset(mask.data(), w, h);
        let rects = bitset_to_rects(&bits, w, h, bb.left, bb.top);
        self.set_rects(rects);
        Ok(())
    }

    pub fn contains(&self, x: i32, y: i32) -> bool {
        self.rects.iter().any(|r| r.contains(x, y))
    }

    pub fn quick_reject(&self, r: Rect) -> bool {
        match self.bounds {
            None => true,
            Some(b) => {
                r.right <= b.left || r.left >= b.right || r.bottom <= b.top || r.top >= b.bottom
            }
        }
    }

    /// `Region.op(other, op)`. Returns whether the result is non-empty.
    pub fn op(&mut self, other: &Region, op: RegionOp) -> Result<bool, RenderError> {
        if op == RegionOp::Replace {
            self.set_region(other);
            return Ok(!self.is_empty());
        }
        let ub = match (self.bounds, other.bounds) {
            (None, None) => None,
            (Some(b), None) | (None, Some(b)) => Some(b),
            (Some(a), Some(b)) => Some(Rect::new(
                a.left.min(b.left),
                a.top.min(b.top),
                a.right.max(b.right),
                a.bottom.max(b.bottom),
            )),
        };
        let Some(ub) = ub else {
            self.set_rects(vec![]);
            return Ok(false);
        };
        let w = ub.width() as u32;
        let h = ub.height() as u32;
        if (w as i64) * (h as i64) > MAX_AREA {
            return Err(RenderError::size(format!(
                "Region.op bounds {w}x{h} exceeds {MAX_AREA} pixels"
            )));
        }
        let a = self.to_bitset(ub, w, h);
        let b = other.to_bitset(ub, w, h);
        let mut out = vec![0u64; a.len()];
        for i in 0..out.len() {
            out[i] = match op {
                RegionOp::Difference => a[i] & !b[i],
                RegionOp::Intersect => a[i] & b[i],
                RegionOp::Union => a[i] | b[i],
                RegionOp::Xor => a[i] ^ b[i],
                RegionOp::ReverseDifference => b[i] & !a[i],
                RegionOp::Replace => b[i],
            };
        }
        self.set_rects(bitset_to_rects(&out, w, h, ub.left, ub.top));
        Ok(!self.is_empty())
    }

    /// `Region.translate(dx, dy) -> dst`.
    pub fn translated(&self, dx: i32, dy: i32) -> Region {
        let mut r = self.clone();
        for rect in r.rects.iter_mut() {
            rect.left += dx;
            rect.right += dx;
            rect.top += dy;
            rect.bottom += dy;
        }
        if let Some(b) = r.bounds.as_mut() {
            b.left += dx;
            b.right += dx;
            b.top += dy;
            b.bottom += dy;
        }
        r
    }

    /// `Region.scale(scale) -> dst` (origin-anchored, rounded).
    pub fn scaled(&self, scale: f32) -> Region {
        let mut r = Region::new();
        let rects: Vec<Rect> = self
            .rects
            .iter()
            .map(|x| {
                Rect::new(
                    (x.left as f32 * scale) as i32,
                    (x.top as f32 * scale) as i32,
                    (x.right as f32 * scale) as i32,
                    (x.bottom as f32 * scale) as i32,
                )
            })
            .collect();
        r.set_rects(rects);
        r
    }

    /// Boundary path (for `Region.getBoundaryPath`). Returns the union of the
    /// individual rectangle outlines.
    pub fn boundary_path(&self) -> Path {
        let mut p = Path::new();
        for r in &self.rects {
            p.add_rect(
                r.left as f32,
                r.top as f32,
                r.right as f32,
                r.bottom as f32,
                false,
            );
        }
        p
    }

    fn to_bitset(&self, ub: Rect, w: u32, h: u32) -> Vec<u64> {
        let mut bits = vec![0u64; (w as usize * h as usize).div_ceil(64)];
        for r in &self.rects {
            let l = (r.left - ub.left).max(0);
            let t = (r.top - ub.top).max(0);
            let rr = (r.right - ub.left).min(w as i32);
            let bb = (r.bottom - ub.top).min(h as i32);
            for y in t..bb {
                for x in l..rr {
                    let idx = (y as usize) * (w as usize) + x as usize;
                    bits[idx / 64] |= 1u64 << (idx % 64);
                }
            }
        }
        bits
    }
}

fn mask_to_bitset(data: &[u8], w: u32, h: u32) -> Vec<u64> {
    let mut bits = vec![0u64; (w as usize * h as usize).div_ceil(64)];
    for (i, &a) in data.iter().enumerate() {
        if a > 0 {
            bits[i / 64] |= 1u64 << (i % 64);
        }
    }
    bits
}

fn bitset_to_rects(bits: &[u64], w: u32, h: u32, ox: i32, oy: i32) -> Vec<Rect> {
    let mut rects = Vec::new();
    let width = w as usize;
    // Greedy maximal-rectangle scanline extraction.
    let mut visited = vec![false; width * h as usize];
    let get = |x: usize, y: usize| -> bool {
        let idx = y * width + x;
        bits[idx / 64] & (1u64 << (idx % 64)) != 0
    };
    for y in 0..h as usize {
        let mut x = 0usize;
        while x < width {
            if !visited[y * width + x] && get(x, y) {
                // Extend right.
                let mut x2 = x;
                while x2 < width && get(x2, y) && !visited[y * width + x2] {
                    x2 += 1;
                }
                // Extend down while all cells in [x, x2) are set and unvisited.
                let mut y2 = y + 1;
                'down: while y2 < h as usize {
                    for xx in x..x2 {
                        if !get(xx, y2) || visited[y2 * width + xx] {
                            break 'down;
                        }
                    }
                    y2 += 1;
                }
                for yy in y..y2 {
                    for xx in x..x2 {
                        visited[yy * width + xx] = true;
                    }
                }
                rects.push(Rect::new(
                    ox + x as i32,
                    oy + y as i32,
                    ox + x2 as i32,
                    oy + y2 as i32,
                ));
                x = x2;
            } else {
                x += 1;
            }
        }
    }
    rects
}

impl Region {
    pub fn from_rectf(r: RectF) -> Self {
        Region::from_rect(r.round_out())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn union_of_two_rects() {
        let mut a = Region::from_rect(Rect::new(0, 0, 10, 10));
        let b = Region::from_rect(Rect::new(5, 0, 15, 10));
        assert!(a.op(&b, RegionOp::Union).unwrap());
        assert_eq!(a.bounds(), Some(Rect::new(0, 0, 15, 10)));
        assert!(a.contains(12, 5));
        assert!(!a.contains(16, 5));
    }

    #[test]
    fn intersect_disjoint_is_empty() {
        let mut a = Region::from_rect(Rect::new(0, 0, 10, 10));
        let b = Region::from_rect(Rect::new(20, 20, 30, 30));
        assert!(!a.op(&b, RegionOp::Intersect).unwrap());
        assert!(a.is_empty());
    }

    #[test]
    fn difference_carves_hole() {
        let mut a = Region::from_rect(Rect::new(0, 0, 10, 10));
        let b = Region::from_rect(Rect::new(0, 0, 10, 5));
        assert!(a.op(&b, RegionOp::Difference).unwrap());
        assert_eq!(a.bounds(), Some(Rect::new(0, 5, 10, 10)));
        assert!(!a.contains(5, 2));
        assert!(a.contains(5, 7));
    }

    #[test]
    fn quick_reject() {
        let a = Region::from_rect(Rect::new(0, 0, 10, 10));
        assert!(a.quick_reject(Rect::new(20, 20, 30, 30)));
        assert!(!a.quick_reject(Rect::new(5, 5, 8, 8)));
    }

    #[test]
    fn set_path_from_rect() {
        let p = Path::from_rect(RectF::new(0.0, 0.0, 10.0, 10.0));
        let mut r = Region::new();
        r.set_path(&p, None).unwrap();
        assert_eq!(r.bounds(), Some(Rect::new(0, 0, 10, 10)));
    }
}
