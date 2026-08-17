use crate::local::Embedded;
use crate::session::plan::{find_local, HashInfo, HashKey, LocalFile, PatchInfo};
use crate::session::source::{hash_of_item, SourceCtx};

const SMALL_FILE: u64 = 500 * 1024;
const MAX_GROUP: usize = 10 * 1024 * 1024;
const MAX_WASTE: f64 = 0.2;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileMode {
    Local,
    Hybrid,
    Patch,
    Direct,
}

#[derive(Debug, Clone)]
pub struct FilePos {
    pub item: HashInfo,
    pub offset: usize,
    pub size: usize,
}

#[derive(Debug, Clone)]
pub enum InstallTask {
    Single(HashInfo),
    Merged {
        files: Vec<FilePos>,
        range: String,
        start: usize,
        download_size: usize,
    },
}

pub fn file_mode(
    item: &HashInfo,
    hash_key: HashKey,
    local: &[Embedded],
    patches: &[PatchInfo],
    skip_patch: bool,
) -> FileMode {
    if skip_patch {
        return FileMode::Direct;
    }
    let Some(hash) = hash_of_item(item, hash_key) else {
        return FileMode::Direct;
    };
    if local.iter().any(|l| l.name == hash) {
        return FileMode::Local;
    }
    let hybrid = patches.iter().any(|p| {
        side_eq(&p.to, hash_key, &hash)
            && side_hash(&p.from, hash_key).is_some_and(|from| local.iter().any(|l| l.name == from))
    });
    if hybrid {
        return FileMode::Hybrid;
    }
    if patches.iter().any(|p| side_eq(&p.to, hash_key, &hash)) {
        return FileMode::Patch;
    }
    FileMode::Direct
}

fn side_hash(side: &crate::session::plan::PatchSide, key: HashKey) -> Option<&str> {
    match key {
        HashKey::Md5 => side.md5.as_deref(),
        HashKey::Xxh => side.xxh.as_deref(),
    }
}

fn side_eq(side: &crate::session::plan::PatchSide, key: HashKey, hash: &str) -> bool {
    side_hash(side, key) == Some(hash)
}

pub fn plan_tasks(
    items: &[HashInfo],
    hash_key: HashKey,
    local: &[Embedded],
    patches: &[PatchInfo],
    ctx: &SourceCtx,
) -> Vec<InstallTask> {
    let mut mergeable = Vec::new();
    let mut non_mergeable = Vec::new();
    for item in items {
        match file_mode(item, hash_key, local, patches, false) {
            FileMode::Direct | FileMode::Patch => mergeable.push(item.clone()),
            _ => non_mergeable.push(item.clone()),
        }
    }

    let mut positioned = Vec::new();
    let mut unindexed = Vec::new();
    let mut large = Vec::new();
    for item in mergeable {
        if item.size > SMALL_FILE {
            large.push(item);
            continue;
        }
        if let Some(file) = hash_of_item(&item, hash_key).and_then(|h| ctx.find(&h).cloned()) {
            positioned.push(FilePos {
                item,
                offset: file.offset,
                size: file.size,
            });
        } else {
            unindexed.push(item);
        }
    }
    positioned.sort_by_key(|p| p.offset);

    let mut singles = Vec::new();
    let mut merged = Vec::new();
    let mut current: Vec<FilePos> = Vec::new();
    for file in positioned {
        if can_merge(&current, &file) {
            current.push(file);
        } else {
            push_group(current, &mut singles, &mut merged);
            current = vec![file];
        }
    }
    push_group(current, &mut singles, &mut merged);

    singles.extend(large);
    singles.extend(unindexed);

    non_mergeable.sort_by(|a, b| b.size.cmp(&a.size));
    singles.sort_by(|a, b| b.size.cmp(&a.size));
    merged.sort_by(|a, b| match (a, b) {
        (
            InstallTask::Merged {
                download_size: da, ..
            },
            InstallTask::Merged {
                download_size: db, ..
            },
        ) => db.cmp(da),
        _ => std::cmp::Ordering::Equal,
    });

    let mut out = Vec::new();
    let mut i = 0;
    let mut j = 0;
    let mut k = 0;
    while i < non_mergeable.len() || j < singles.len() || k < merged.len() {
        if i < non_mergeable.len() {
            out.push(InstallTask::Single(non_mergeable[i].clone()));
            i += 1;
        }
        if j < singles.len() {
            out.push(InstallTask::Single(singles[j].clone()));
            j += 1;
        }
        if k < merged.len() {
            out.push(merged[k].clone());
            k += 1;
        }
    }
    out
}

fn can_merge(group: &[FilePos], new: &FilePos) -> bool {
    if group.is_empty() {
        return true;
    }
    let last = group.last().unwrap();
    let group_end = last.offset + last.size;
    if new.offset < group_end {
        return false;
    }
    let start = group[0].offset;
    let end = new.offset + new.size;
    let total = end - start;
    if total > MAX_GROUP {
        return false;
    }
    let effective = group.iter().map(|f| f.size).sum::<usize>() + new.size;
    let waste = (total - effective) as f64 / total as f64;
    waste <= MAX_WASTE
}

fn push_group(group: Vec<FilePos>, singles: &mut Vec<HashInfo>, merged: &mut Vec<InstallTask>) {
    if group.is_empty() {
        return;
    }
    if group.len() == 1 {
        singles.push(group.into_iter().next().unwrap().item);
        return;
    }
    let start = group[0].offset;
    let last = group.last().unwrap();
    let end = last.offset + last.size;
    let download_size = end - start;
    merged.push(InstallTask::Merged {
        range: format!("{}-{}", start, end.saturating_sub(1)),
        start,
        download_size,
        files: group,
    });
}

pub fn dfs2_ranges(
    tasks: &[InstallTask],
    ctx: &SourceCtx,
    hash_key: HashKey,
    embedded: &[Embedded],
    patches: &[PatchInfo],
    disk: &[LocalFile],
) -> Vec<String> {
    let mut ranges = Vec::new();
    for task in tasks {
        match task {
            InstallTask::Merged { range, files, .. } => {
                ranges.push(range.clone());
                for file in files {
                    add_file_ranges(
                        &mut ranges,
                        &file.item,
                        ctx,
                        hash_key,
                        embedded,
                        patches,
                        disk,
                    );
                }
            }
            InstallTask::Single(item) => {
                add_file_ranges(&mut ranges, item, ctx, hash_key, embedded, patches, disk);
            }
        }
    }
    ranges.sort();
    ranges.dedup();
    ranges
}

fn add_file_ranges(
    ranges: &mut Vec<String>,
    item: &HashInfo,
    ctx: &SourceCtx,
    hash_key: HashKey,
    embedded: &[Embedded],
    patches: &[PatchInfo],
    disk: &[LocalFile],
) {
    let Some(hash) = hash_of_item(item, hash_key) else {
        add_installer_range(ranges, item, ctx);
        return;
    };
    if embedded.iter().any(|file| file.name == hash) {
        return;
    }

    let hybrid = patches.iter().find(|patch| {
        side_hash(&patch.to, hash_key) == Some(hash.as_str())
            && side_hash(&patch.from, hash_key)
                .is_some_and(|from| embedded.iter().any(|file| file.name == from))
    });
    if let Some(patch) = hybrid {
        if let Some(from) = side_hash(&patch.from, hash_key) {
            add_index_range(ranges, ctx, &format!("{from}_{hash}"));
        }
        add_index_range(ranges, ctx, &hash);
    } else {
        let disk_hash = find_local(disk, &item.file_name).map(|file| file.hash.as_str());
        let patch = patches.iter().find(|patch| {
            side_hash(&patch.to, hash_key) == Some(hash.as_str())
                && side_hash(&patch.from, hash_key) == disk_hash
        });
        if let Some(patch) = patch {
            if let Some(from) = side_hash(&patch.from, hash_key) {
                add_index_range(ranges, ctx, &format!("{from}_{hash}"));
            }
            add_index_range(ranges, ctx, &hash);
        } else {
            add_index_range(ranges, ctx, &hash);
        }
    }
    add_installer_range(ranges, item, ctx);
}

fn add_index_range(ranges: &mut Vec<String>, ctx: &SourceCtx, hash: &str) {
    if let Some(file) = ctx.find(hash) {
        let end = file.offset + file.size.saturating_sub(1);
        ranges.push(format!("{}-{}", file.offset, end));
    }
}

fn add_installer_range(ranges: &mut Vec<String>, item: &HashInfo, ctx: &SourceCtx) {
    if item.installer.unwrap_or(false) && ctx.installer_end > 0 {
        ranges.push(format!("0-{}", ctx.installer_end.saturating_sub(1)));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::plan::{LocalFile, PatchInfo, PatchSide};

    fn emb(name: &str, offset: usize, size: usize) -> Embedded {
        Embedded {
            name: name.to_string(),
            offset,
            raw_offset: 0,
            size,
        }
    }

    fn item(name: &str, hash: &str, installer: bool) -> HashInfo {
        HashInfo {
            file_name: name.to_string(),
            size: 10,
            md5: Some(hash.to_string()),
            xxh: None,
            installer: Some(installer).filter(|v| *v),
        }
    }

    fn patch(from: &str, to: &str) -> PatchInfo {
        PatchInfo {
            file_name: "app.exe".to_string(),
            size: 8,
            from: PatchSide {
                size: 1,
                md5: Some(from.to_string()),
                xxh: None,
            },
            to: PatchSide {
                size: 1,
                md5: Some(to.to_string()),
                xxh: None,
            },
        }
    }

    #[test]
    fn skip_ranges_for_embedded_files() {
        let ctx = SourceCtx::from_embedded(&[]);
        let embedded = vec![emb("bbb", 0, 10)];
        let tasks = vec![InstallTask::Single(item("app.exe", "bbb", false))];
        let ranges = dfs2_ranges(&tasks, &ctx, HashKey::Md5, &embedded, &[], &[]);
        assert!(ranges.is_empty());
    }

    #[test]
    fn patch_declares_delta_and_full_file() {
        let mut ctx = SourceCtx::from_embedded(&[]);
        ctx.restore_local_package(
            Some(&[emb("bbb", 100, 50), emb("aaa_bbb", 200, 21)]),
            None,
        );
        let tasks = vec![InstallTask::Single(item("app.exe", "bbb", false))];
        let disk = vec![LocalFile {
            file_name: "app.exe".to_string(),
            hash: "aaa".to_string(),
            size: 1,
            unwritable: false,
        }];
        let ranges = dfs2_ranges(
            &tasks,
            &ctx,
            HashKey::Md5,
            &[],
            &[patch("aaa", "bbb")],
            &disk,
        );
        assert_eq!(ranges, vec!["100-149".to_string(), "200-220".to_string()]);
    }

    #[test]
    fn installer_declares_prefix() {
        let mut ctx = SourceCtx::from_embedded(&[]);
        ctx.restore_local_package(Some(&[emb("upd", 80, 10)]), None);
        ctx.installer_end = 80;
        let tasks = vec![InstallTask::Single(item("updater.exe", "upd", true))];
        let ranges = dfs2_ranges(&tasks, &ctx, HashKey::Md5, &[], &[], &[]);
        assert_eq!(ranges, vec!["0-79".to_string(), "80-89".to_string()]);
    }
}
