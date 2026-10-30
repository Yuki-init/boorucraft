//! Near-duplicate detection. Pure functions, no IO and no image decoding: the caller scales a
//! picture down to a grid of grey values and hands it in.
//!
//! - [`dhash`]: 9×8 grid, one bit per pair of neighbours (is the left one darker?).
//! - [`phash`]: 32×32 grid, the low 8×8 frequencies of its DCT, one bit per coefficient (is it
//!   above the median?).
//! - [`BkTree`]: finds hashes within a Hamming distance without comparing every pair.
//! - [`groups`]: pictures whose two hashes are both close, joined into groups.

#![cfg_attr(not(test), deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable))]

/// Width and height of the grid [`dhash`] takes.
pub const DHASH_SIZE: (usize, usize) = (9, 8);
/// Side of the grid [`phash`] takes.
pub const PHASH_SIZE: usize = 32;
/// Default most bits two hashes may differ by and still count as duplicates.
pub const DEFAULT_THRESHOLD: u32 = 6;
/// Largest threshold accepted (beyond this, unrelated pictures match).
pub const MAX_THRESHOLD: u32 = 20;

/// Difference hash of a 9×8 grid of grey values, row by row. `None` if the grid has the wrong size.
pub fn dhash(gray: &[f32]) -> Option<u64> {
    let (w, h) = DHASH_SIZE;
    if gray.len() != w * h {
        return None;
    }
    let mut bits = 0u64;
    for row in gray.chunks_exact(w) {
        for pair in row.windows(2) {
            bits <<= 1;
            if let [a, b] = pair
                && a < b
            {
                bits |= 1;
            }
        }
    }
    Some(bits)
}

/// Perceptual (DCT) hash of a 32×32 grid of grey values, row by row. `None` if the grid has the
/// wrong size.
pub fn phash(gray: &[f32]) -> Option<u64> {
    const N: usize = PHASH_SIZE;
    const K: usize = 8;
    if gray.len() != N * N {
        return None;
    }
    // cos[k][x] for the DCT-II.
    let mut cos = [[0f32; N]; K];
    for (k, row) in cos.iter_mut().enumerate() {
        for (x, c) in row.iter_mut().enumerate() {
            *c = (std::f32::consts::PI / N as f32 * (x as f32 + 0.5) * k as f32).cos();
        }
    }
    // Rows first (only the K lowest frequencies are needed), then columns.
    let mut rows = [[0f32; K]; N];
    for (y, out) in rows.iter_mut().enumerate() {
        let line = gray.get(y * N..(y + 1) * N)?;
        for (k, o) in out.iter_mut().enumerate() {
            *o = line.iter().zip(&cos[k]).map(|(v, c)| v * c).sum();
        }
    }
    let mut coef = [0f32; K * K];
    for v in 0..K {
        for u in 0..K {
            coef[v * K + u] = (0..N).map(|y| rows[y][u] * cos[v][y]).sum();
        }
    }
    // The median of everything but the DC term (which is just the average brightness).
    let mut sorted: Vec<f32> = coef.iter().skip(1).copied().collect();
    sorted.sort_by(f32::total_cmp);
    let median = sorted.get(sorted.len() / 2).copied().unwrap_or(0.0);
    Some(coef.iter().fold(0u64, |bits, &c| (bits << 1) | u64::from(c > median)))
}

/// Number of bits that differ.
pub fn distance(a: u64, b: u64) -> u32 {
    (a ^ b).count_ones()
}

/// 16 hex digits, as stored in collection.json.
pub fn to_hex(h: u64) -> String {
    format!("{h:016x}")
}

pub fn from_hex(s: &str) -> Option<u64> {
    if s.len() == 16 { u64::from_str_radix(s, 16).ok() } else { None }
}

#[derive(Debug)]
struct Node {
    hash: u64,
    items: Vec<usize>,
    /// (distance from this node's hash, child node).
    children: Vec<(u32, usize)>,
}

/// A BK-tree over 64-bit hashes with the Hamming distance.
#[derive(Debug, Default)]
pub struct BkTree {
    nodes: Vec<Node>,
}

impl BkTree {
    pub fn new() -> BkTree {
        BkTree::default()
    }

    pub fn insert(&mut self, hash: u64, item: usize) {
        if self.nodes.is_empty() {
            self.nodes.push(Node { hash, items: vec![item], children: Vec::new() });
            return;
        }
        let mut at = 0;
        loop {
            let next_id = self.nodes.len();
            let Some(node) = self.nodes.get_mut(at) else { return };
            let d = distance(node.hash, hash);
            if d == 0 {
                node.items.push(item);
                return;
            }
            match node.children.iter().find(|(cd, _)| *cd == d) {
                Some(&(_, child)) => at = child,
                None => {
                    node.children.push((d, next_id));
                    self.nodes.push(Node { hash, items: vec![item], children: Vec::new() });
                    return;
                }
            }
        }
    }

    /// Items whose hash is within `radius` of `hash`.
    pub fn find(&self, hash: u64, radius: u32) -> Vec<usize> {
        let mut out = Vec::new();
        let mut stack = if self.nodes.is_empty() { Vec::new() } else { vec![0usize] };
        while let Some(at) = stack.pop() {
            let Some(node) = self.nodes.get(at) else { continue };
            let d = distance(node.hash, hash);
            if d <= radius {
                out.extend_from_slice(&node.items);
            }
            let (lo, hi) = (d.saturating_sub(radius), d.saturating_add(radius));
            stack.extend(node.children.iter().filter(|(cd, _)| (lo..=hi).contains(cd)).map(|(_, c)| *c));
        }
        out
    }
}

/// The two hashes of one picture.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Hashes {
    pub dhash: u64,
    pub phash: u64,
}

/// Groups of near-duplicates: two pictures are joined when both their dHash and pHash differ by
/// at most `threshold` bits, unless `keep_apart(a, b)` says the user decided they aren't
/// duplicates. Groups (of indexes into `items`, at least two each) are sorted, and so is each
/// group.
pub fn groups(items: &[Hashes], threshold: u32, keep_apart: impl Fn(usize, usize) -> bool) -> Vec<Vec<usize>> {
    let threshold = threshold.min(MAX_THRESHOLD);
    let mut tree = BkTree::new();
    for (i, h) in items.iter().enumerate() {
        tree.insert(h.dhash, i);
    }
    let mut parent: Vec<usize> = (0..items.len()).collect();
    fn root(parent: &mut [usize], mut i: usize) -> usize {
        // Path halving; bounded because every step moves closer to a root.
        while let Some(&p) = parent.get(i) {
            if p == i {
                break;
            }
            let gp = parent.get(p).copied().unwrap_or(p);
            if let Some(slot) = parent.get_mut(i) {
                *slot = gp;
            }
            i = gp;
        }
        i
    }
    for (i, h) in items.iter().enumerate() {
        for j in tree.find(h.dhash, threshold) {
            if j <= i {
                continue;
            }
            let Some(other) = items.get(j) else { continue };
            if distance(h.phash, other.phash) > threshold || keep_apart(i, j) {
                continue;
            }
            let (a, b) = (root(&mut parent, i), root(&mut parent, j));
            if a != b
                && let Some(slot) = parent.get_mut(a.max(b))
            {
                *slot = a.min(b);
            }
        }
    }
    let mut by_root: std::collections::BTreeMap<usize, Vec<usize>> = std::collections::BTreeMap::new();
    for i in 0..items.len() {
        let r = root(&mut parent, i);
        by_root.entry(r).or_default().push(i);
    }
    by_root.into_values().filter(|g| g.len() > 1).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A smooth test picture of any size: a diagonal gradient with a bright blob.
    fn picture(w: usize, h: usize, shift: f32) -> Vec<f32> {
        let mut out = Vec::with_capacity(w * h);
        for y in 0..h {
            for x in 0..w {
                let (fx, fy) = (x as f32 / w as f32, y as f32 / h as f32);
                let blob = (-((fx - 0.3 - shift).powi(2) + (fy - 0.6).powi(2)) * 20.0).exp();
                out.push(fx * 100.0 + fy * 40.0 + blob * 120.0);
            }
        }
        out
    }

    #[test]
    fn similar_pictures_have_close_hashes() {
        let a = (dhash(&picture(9, 8, 0.0)).unwrap(), phash(&picture(32, 32, 0.0)).unwrap());
        // Slightly brighter and a little noise: still the same picture.
        let brighter: Vec<f32> = picture(32, 32, 0.0).iter().enumerate().map(|(i, v)| v * 1.1 + 5.0 + (i % 3) as f32 * 0.5).collect();
        let b = phash(&brighter).unwrap();
        assert!(distance(a.1, b) <= 4, "pHash survives brightness changes: {}", distance(a.1, b));
        // A different picture.
        let mirrored: Vec<f32> = picture(32, 32, 0.0).chunks(32).flat_map(|r| r.iter().rev().copied().collect::<Vec<_>>()).collect();
        assert!(distance(a.1, phash(&mirrored).unwrap()) > 16);
        let mirrored9: Vec<f32> = picture(9, 8, 0.0).chunks(9).flat_map(|r| r.iter().rev().copied().collect::<Vec<_>>()).collect();
        assert!(distance(a.0, dhash(&mirrored9).unwrap()) > 32);
        assert_eq!(dhash(&[0.0; 3]), None);
        assert_eq!(phash(&[0.0; 3]), None);
        assert_eq!(from_hex(&to_hex(a.1)), Some(a.1));
        assert_eq!(from_hex("xyz"), None);
    }

    #[test]
    fn bk_tree_finds_everything_within_the_radius() {
        let mut tree = BkTree::new();
        let hashes: Vec<u64> = (0..500u64).map(|i| i.wrapping_mul(0x9e37_79b9_7f4a_7c15)).collect();
        for (i, h) in hashes.iter().enumerate() {
            tree.insert(*h, i);
        }
        tree.insert(hashes[7], 999);
        for (probe, radius) in [(hashes[7], 0), (hashes[7] ^ 0b1011, 3), (hashes[123] ^ 0xff, 10), (0, 30)] {
            let mut found = tree.find(probe, radius);
            found.sort_unstable();
            let mut want: Vec<usize> = hashes.iter().enumerate().filter(|(_, h)| distance(**h, probe) <= radius).map(|(i, _)| i).collect();
            if distance(hashes[7], probe) <= radius {
                want.push(999);
            }
            want.sort_unstable();
            assert_eq!(found, want, "probe {probe:x} radius {radius}");
        }
        assert!(BkTree::new().find(1, 64).is_empty());
    }

    #[test]
    fn groups_join_close_pictures_and_respect_decisions() {
        let h = |d: u64, p: u64| Hashes { dhash: d, phash: p };
        let items = [
            h(0, 0),                   // 0
            h(0b111, 0b11),            // 1: close to 0
            h(u64::MAX, u64::MAX),     // 2: far from everything
            h(0b1111_1111, 0b111),     // 3: close to 1 (so joins 0's group)
            h(0, u64::MAX),            // 4: same dHash as 0, different pHash
            h(u64::MAX, u64::MAX - 1), // 5: close to 2
        ];
        assert_eq!(groups(&items, 8, |_, _| false), vec![vec![0, 1, 3], vec![2, 5]]);
        assert_eq!(groups(&items, 0, |_, _| false), Vec::<Vec<usize>>::new());
        let apart = |a: usize, b: usize| (a, b) == (2, 5);
        assert_eq!(groups(&items, 8, apart), vec![vec![0, 1, 3]], "marked not duplicates");
        assert!(groups(&[], 8, |_, _| false).is_empty());
    }
}
