//! 索引范围规划：1MiB 文件分类、补丁取舍、合并均衡及等长切片。
//! 不读取包内容，不解析下载地址；输出中的文件序号沿用输入，重试不重新编号。

pub const MIB: u64 = 1024 * 1024;
pub const SMALL_FILE: u64 = MIB;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Range {
    pub start: u64,
    pub len: u64,
}

impl Range {
    pub fn end(self) -> u64 {
        self.start + self.len
    }

    pub fn key(self) -> String {
        format!("{}-{}", self.start, self.end() - 1)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Policy {
    pub lower: u64,
    pub upper: u64,
    pub maximum: u64,
    pub concurrency: usize,
}

impl Default for Policy {
    fn default() -> Self {
        Self {
            lower: 10 * MIB,
            upper: 25 * MIB,
            maximum: 50 * MIB,
            concurrency: 16,
        }
    }
}

impl Policy {
    /// 大小建议必须整组有效；并发独立处理。缩放只限制资源，不改变包格式。
    pub fn from_hints(
        maximum: Option<u64>,
        upper: Option<u64>,
        lower: Option<u64>,
        concurrency: Option<usize>,
    ) -> Self {
        let mut policy = Self::default();
        if let (Some(maximum), Some(upper), Some(lower)) = (maximum, upper, lower) {
            if lower > 0 && lower <= upper && upper <= maximum {
                let cap = maximum.min(256 * MIB);
                let scale = |n| (u128::from(n) * u128::from(cap) / u128::from(maximum)) as u64;
                let (lower, upper) = (scale(lower), scale(upper));
                if lower >= MIB {
                    policy.lower = lower;
                    policy.upper = upper;
                    policy.maximum = cap;
                }
            }
        }
        if let Some(n) = concurrency.filter(|n| *n > 0) {
            policy.concurrency = n.min(16);
        }
        policy
    }

    pub fn split(self, range: Range) -> Vec<Range> {
        if range.len == 0 {
            return Vec::new();
        }
        let count = range
            .len
            .div_ceil(self.maximum)
            .max(range.len / self.upper)
            .max(1);
        let (size, remainder) = (range.len / count, range.len % count);
        let mut start = range.start;
        (0..count)
            .map(|i| {
                let len = size + u64::from(i < remainder);
                let part = Range { start, len };
                start += len;
                part
            })
            .collect()
    }
}

#[derive(Debug, Clone, Copy)]
pub struct Input {
    pub id: usize,
    pub raw_size: u64,
    pub full: Range,
    pub patch: Option<Range>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Selection {
    pub id: usize,
    pub range: Range,
    pub patch: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Transfer {
    pub files: Vec<Selection>,
    pub parts: Vec<Range>,
}

fn full(input: &Input) -> Selection {
    Selection {
        id: input.id,
        range: input.full,
        patch: false,
    }
}

fn preferred(input: &Input) -> Selection {
    Selection {
        id: input.id,
        range: input.patch.unwrap_or(input.full),
        patch: input.patch.is_some(),
    }
}

fn extent(group: &[Selection]) -> u64 {
    group.last().unwrap().range.end() - group[0].range.start
}

fn allowed(length: u64, useful: u64) -> bool {
    u128::from(length - useful) * 5 <= u128::from(length)
}

fn groups(mut items: Vec<Selection>, policy: Policy, balanced: bool) -> Vec<Vec<Selection>> {
    items.sort_by_key(|i| i.range.start);
    let mut out = Vec::new();
    let mut current: Vec<Selection> = Vec::new();
    let mut useful = 0;
    for item in items {
        if let Some(last) = current.last() {
            let length = item.range.end().saturating_sub(current[0].range.start);
            let overlaps = item.range.start < last.range.end();
            let past_target = length > policy.lower
                && extent(&current).abs_diff(policy.lower) < length.abs_diff(policy.lower);
            if overlaps
                || length > policy.upper
                || !allowed(length, useful + item.range.len)
                || past_target
            {
                out.push(std::mem::take(&mut current));
                useful = 0;
            }
        }
        useful += item.range.len;
        current.push(item);
        if extent(&current) >= policy.lower {
            out.push(std::mem::take(&mut current));
            useful = 0;
        }
    }
    if !current.is_empty() {
        out.push(current);
    }
    if balanced {
        balance(out, policy)
    } else {
        out
    }
}

fn balance(groups: Vec<Vec<Selection>>, policy: Policy) -> Vec<Vec<Selection>> {
    if groups.len() < 2 {
        return groups;
    }
    let mut items = Vec::new();
    let mut bounds = Vec::new();
    for group in groups {
        let begin = items.len();
        items.extend(group);
        bounds.push((begin, items.len()));
    }
    let mut useful = vec![0u64];
    let mut overlaps = vec![0usize];
    for (i, item) in items.iter().enumerate() {
        useful.push(useful[i] + item.range.len);
        overlaps
            .push(overlaps[i] + usize::from(i > 0 && item.range.start < items[i - 1].range.end()));
    }
    let size = |a: usize, b: usize| items[b - 1].range.end() - items[a].range.start;
    let valid = |a: usize, b: usize| {
        size(a, b) <= policy.upper && allowed(size(a, b), useful[b] - useful[a])
    };
    for i in 0..bounds.len() - 1 {
        let (begin, middle) = bounds[i];
        let (_, end) = bounds[i + 1];
        if begin == middle || overlaps[end] != overlaps[begin + 1] {
            continue;
        }
        if size(begin, end) <= policy.lower && valid(begin, end) {
            bounds[i] = (begin, begin);
            bounds[i + 1] = (begin, end);
            continue;
        }
        let mut candidates = vec![middle];
        for offset in [
            (items[begin].range.start + items[end - 1].range.end()) / 2,
            items[begin].range.start + policy.lower,
            items[end - 1].range.end().saturating_sub(policy.lower),
        ] {
            let cut = begin + items[begin..end].partition_point(|item| item.range.start < offset);
            candidates.extend([cut.saturating_sub(1), cut, cut + 1]);
        }
        candidates.sort_unstable();
        candidates.dedup();
        let original_bytes = size(begin, middle) + size(middle, end);
        let score = |cut| {
            let (a, b) = (size(begin, cut), size(cut, end));
            (
                a.min(b),
                std::cmp::Reverse(a.max(b)),
                std::cmp::Reverse(a + b),
            )
        };
        let mut best = middle;
        for cut in candidates {
            if begin < cut
                && cut < end
                && valid(begin, cut)
                && valid(cut, end)
                && size(begin, cut) + size(cut, end) <= original_bytes
                && score(cut) > score(best)
            {
                best = cut;
            }
        }
        bounds[i] = (begin, best);
        bounds[i + 1] = (best, end);
    }
    bounds
        .into_iter()
        .filter(|(a, b)| a < b)
        .map(|(a, b)| items[a..b].to_vec())
        .collect()
}

/// 输入为同一远端包内可按范围读取的文件；本地、混合基文件和未知范围由调用者保留单流。
/// 相同 payload 的多个目标分别保留，不对目标文件去重。
pub fn plan(inputs: &[Input], policy: Policy) -> Vec<Transfer> {
    let by_id: std::collections::HashMap<_, _> =
        inputs.iter().map(|input| (input.id, input)).collect();
    let small = inputs
        .iter()
        .filter(|i| i.raw_size <= SMALL_FILE)
        .map(full)
        .collect();
    let mut selected = Vec::new();
    for group in groups(small, policy, true) {
        if group.len() == 1 {
            selected.push(vec![preferred(by_id[&group[0].id])]);
        } else if group.iter().any(|i| by_id[&i.id].patch.is_some()) {
            let alternatives = groups(
                group.iter().map(|i| preferred(by_id[&i.id])).collect(),
                policy,
                true,
            );
            let cost = (
                alternatives.len(),
                alternatives.iter().map(|g| extent(g)).sum::<u64>(),
            );
            if cost < (1, extent(&group)) {
                selected.extend(alternatives);
            } else {
                selected.push(group);
            }
        } else {
            selected.push(group);
        }
    }
    selected.extend(
        inputs
            .iter()
            .filter(|i| i.raw_size > SMALL_FILE)
            .map(|i| vec![preferred(i)]),
    );
    let mut patches = Vec::new();
    selected.retain(|group| {
        if group.len() == 1 && group[0].patch && group[0].range.len <= policy.lower {
            patches.push(group[0]);
            false
        } else {
            true
        }
    });
    selected.extend(groups(patches, policy, false));
    selected
        .into_iter()
        .map(|files| {
            let range = Range {
                start: files[0].range.start,
                len: extent(&files),
            };
            let parts = if files.len() == 1 {
                policy.split(range)
            } else {
                vec![range]
            };
            Transfer { files, parts }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn input(id: usize, start: u64, len: u64) -> Input {
        Input {
            id,
            raw_size: len,
            full: Range { start, len },
            patch: None,
        }
    }

    #[test]
    fn split_boundaries_and_large_offsets() {
        for maximum in [25 * MIB, 50 * MIB] {
            for len in [
                1,
                25 * MIB - 1,
                25 * MIB,
                25 * MIB + 1,
                100 * MIB,
                110 * MIB,
                u32::MAX as u64,
            ] {
                let policy = Policy {
                    maximum,
                    ..Policy::default()
                };
                let range = Range {
                    start: u32::MAX as u64 + 73,
                    len,
                };
                let parts = policy.split(range);
                assert_eq!(parts.iter().map(|p| p.len).sum::<u64>(), len);
                assert_eq!(parts[0].start, range.start);
                assert_eq!(parts.last().unwrap().end(), range.end());
                assert!(parts.windows(2).all(|w| w[0].end() == w[1].start));
                assert!(parts.iter().all(|p| p.len > 0 && p.len <= maximum));
                assert!(
                    parts.iter().map(|p| p.len).max().unwrap()
                        - parts.iter().map(|p| p.len).min().unwrap()
                        <= 1
                );
            }
        }
    }

    #[test]
    fn hints_are_atomic_and_bounded() {
        assert_eq!(
            Policy::from_hints(Some(25 * MIB), None, Some(10 * MIB), None),
            Policy::default()
        );
        assert_eq!(
            Policy::from_hints(Some(10 * MIB), Some(25 * MIB), Some(MIB), None),
            Policy::default()
        );
        let p = Policy::from_hints(Some(512 * MIB), Some(256 * MIB), Some(128 * MIB), Some(100));
        assert_eq!(
            (p.maximum, p.upper, p.lower, p.concurrency),
            (256 * MIB, 128 * MIB, 64 * MIB, 16)
        );
    }

    #[test]
    fn patches_compete_with_full_groups() {
        let mut files = vec![input(0, 1000, 100), input(1, 1110, 100)];
        files[0].patch = Some(Range { start: 0, len: 5 });
        files[1].patch = Some(Range { start: 6, len: 5 });
        let selected = plan(&files, Policy::default());
        assert_eq!(selected.len(), 1);
        assert!(selected[0].files.iter().all(|f| f.patch));
        files[1].patch = Some(Range {
            start: 10000,
            len: 5,
        });
        assert!(plan(&files, Policy::default())[0]
            .files
            .iter()
            .all(|f| !f.patch));
    }

    #[test]
    fn balancing_preserves_gap_savings_and_aliases() {
        let p = Policy {
            lower: 1500,
            upper: 2500,
            maximum: 5000,
            ..Policy::default()
        };
        let files: Vec<_> = (0..26).map(|id| input(id, id as u64 * 100, 100)).collect();
        let out = plan(&files, p);
        assert_eq!(
            out.iter().map(|t| t.files.len()).collect::<Vec<_>>(),
            vec![13, 13]
        );
        let mut files = files;
        for i in &mut files[15..] {
            i.full.start += 300;
        }
        let out = plan(&files, p);
        assert_eq!(
            out.iter().map(|t| t.files.len()).collect::<Vec<_>>(),
            vec![15, 11]
        );
        let alias = vec![input(0, 10, 100), input(1, 10, 100), input(2, 120, 100)];
        assert_eq!(
            plan(&alias, p).iter().map(|t| t.files.len()).sum::<usize>(),
            3
        );
    }

    #[test]
    fn deterministic_layouts_cover_every_file() {
        let mut seed = 97271u64;
        for _ in 0..120 {
            let mut random = || {
                seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1);
                seed
            };
            let mut start = random() % u32::MAX as u64;
            let mut files = Vec::new();
            for id in 0..100 {
                start += random() % 200000;
                let len = random() % 2000000 + 1;
                files.push(input(id, start, len));
                start += len;
            }
            let result = plan(&files, Policy::default());
            assert_eq!(result, plan(&files, Policy::default()));
            let mut ids: Vec<_> = result
                .iter()
                .flat_map(|t| t.files.iter().map(|f| f.id))
                .collect();
            ids.sort_unstable();
            assert_eq!(ids, (0..files.len()).collect::<Vec<_>>());
            for t in result {
                if t.files.len() > 1 {
                    assert!(allowed(
                        extent(&t.files),
                        t.files.iter().map(|f| f.range.len).sum()
                    ));
                    assert!(extent(&t.files) <= 25 * MIB);
                }
            }
        }
    }
}
