//! Line-by-line comparison of two object scripts, aligned as SQL Compare shows them: equal lines
//! side by side, changed lines paired up, and lines present on one side only.

/// One aligned row: a line from each side (`None` where that side has no line).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Line {
    pub left: Option<String>,
    pub right: Option<String>,
    pub different: bool,
}

/// Lines whose normalized forms are equal count as the same line.
fn key(line: &str, ignore_whitespace: bool) -> String {
    if ignore_whitespace { line.split_whitespace().collect::<Vec<_>>().join(" ") } else { line.trim_end().to_string() }
}

/// Above this many cells the middle of the two texts is not aligned line by line.
const MAX_CELLS: usize = 4_000_000;

/// Aligns `left` and `right` (either may be absent: an object on one side only).
pub fn compare(left: Option<&str>, right: Option<&str>, ignore_whitespace: bool) -> Vec<Line> {
    let split = |t: Option<&str>| -> Vec<String> { t.map(|t| t.replace("\r\n", "\n").split('\n').map(str::to_string).collect()).unwrap_or_default() };
    let (a, b) = (split(left), split(right));
    let (ka, kb): (Vec<String>, Vec<String>) = (a.iter().map(|l| key(l, ignore_whitespace)).collect(), b.iter().map(|l| key(l, ignore_whitespace)).collect());
    let prefix = ka.iter().zip(&kb).take_while(|(x, y)| x == y).count();
    let suffix = ka[prefix..].iter().rev().zip(kb[prefix..].iter().rev()).take_while(|(x, y)| x == y).count();
    let (ma, mb) = (&ka[prefix..ka.len() - suffix], &kb[prefix..kb.len() - suffix]);

    // Matched pairs in the middle, by longest common subsequence.
    let mut pairs = Vec::new();
    if !ma.is_empty() && !mb.is_empty() && ma.len() * mb.len() <= MAX_CELLS {
        let (n, m) = (ma.len(), mb.len());
        let mut dp = vec![0u32; (n + 1) * (m + 1)];
        for i in (0..n).rev() {
            for j in (0..m).rev() {
                dp[i * (m + 1) + j] =
                    if ma[i] == mb[j] { dp[(i + 1) * (m + 1) + j + 1] + 1 } else { dp[(i + 1) * (m + 1) + j].max(dp[i * (m + 1) + j + 1]) };
            }
        }
        let (mut i, mut j) = (0, 0);
        while i < n && j < m {
            if ma[i] == mb[j] {
                pairs.push((i, j));
                i += 1;
                j += 1;
            } else if dp[(i + 1) * (m + 1) + j] >= dp[i * (m + 1) + j + 1] {
                i += 1;
            } else {
                j += 1;
            }
        }
    }

    let mut out = Vec::new();
    let same = |out: &mut Vec<Line>, i: usize, j: usize| out.push(Line { left: Some(a[i].clone()), right: Some(b[j].clone()), different: false });
    for k in 0..prefix {
        same(&mut out, k, k);
    }
    // A run of left-only and right-only lines between two matches: pair them up, then the rest.
    let hunk = |out: &mut Vec<Line>, l: std::ops::Range<usize>, r: std::ops::Range<usize>| {
        let (l, r): (Vec<usize>, Vec<usize>) = (l.collect(), r.collect());
        for k in 0..l.len().max(r.len()) {
            out.push(Line { left: l.get(k).map(|&i| a[i].clone()), right: r.get(k).map(|&j| b[j].clone()), different: true });
        }
    };
    let (mut i, mut j) = (0, 0);
    for (pi, pj) in pairs {
        hunk(&mut out, prefix + i..prefix + pi, prefix + j..prefix + pj);
        same(&mut out, prefix + pi, prefix + pj);
        (i, j) = (pi + 1, pj + 1);
    }
    hunk(&mut out, prefix + i..prefix + ma.len(), prefix + j..prefix + mb.len());
    for k in 0..suffix {
        same(&mut out, a.len() - suffix + k, b.len() - suffix + k);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn show(lines: &[Line]) -> Vec<String> {
        lines
            .iter()
            .map(|l| format!("{}{}|{}", if l.different { "*" } else { " " }, l.left.as_deref().unwrap_or("-"), l.right.as_deref().unwrap_or("-")))
            .collect()
    }

    #[test]
    fn aligns_changes_insertions_and_deletions() {
        let a = "CREATE TABLE t\n(\na int,\nb int,\nc int\n)";
        let b = "CREATE TABLE t\n(\na bigint,\nc int\n)";
        assert_eq!(show(&compare(Some(a), Some(b), false)), [
            " CREATE TABLE t|CREATE TABLE t",
            " (|(",
            "*a int,|a bigint,",
            "*b int,|-",
            " c int|c int",
            " )|)",
        ]);
        assert_eq!(show(&compare(Some("x\ny"), None, false)), ["*x|-", "*y|-"]);
        assert!(compare(Some("a  b"), Some("a b"), true).iter().all(|l| !l.different));
    }
}
