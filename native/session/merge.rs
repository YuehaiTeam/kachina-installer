use crate::local::Embedded;
use crate::session::plan::{find_local, HashKey, LocalFile};
use crate::session::source::{hash_of_item, SourceCtx};
use crate::utils::metadata::{FileMeta, PatchInfo, PatchSide};

use super::download_plan::{self, Input, Range};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileMode {
    Local,
    Hybrid,
    Patch,
    Direct,
}

#[derive(Debug, Clone)]
pub struct FilePos {
    pub item: FileMeta,
    pub offset: usize,
    pub size: usize,
    pub patch: Option<PatchInfo>,
}

#[derive(Debug, Clone)]
pub enum InstallTask {
    Single(FileMeta),
    Merged {
        files: Vec<FilePos>,
        range: String,
        start: usize,
        download_size: usize,
    },
}

pub fn file_mode(
    item: &FileMeta,
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

fn side_hash(side: &PatchSide, key: HashKey) -> Option<&str> {
    match key {
        HashKey::Md5 => side.md5.as_deref(),
        HashKey::Xxh => side.xxh.as_deref(),
    }
}

fn side_eq(side: &PatchSide, key: HashKey, hash: &str) -> bool {
    side_hash(side, key) == Some(hash)
}

pub fn plan_tasks(
    items: &[FileMeta],
    hash_key: HashKey,
    local: &[Embedded],
    patches: &[PatchInfo],
    ctx: &SourceCtx,
    disk: &[LocalFile],
) -> Vec<InstallTask> {
    let mut inputs = Vec::new();
    let mut tasks = Vec::new();
    let mut matching = std::collections::HashMap::new();
    for (id, item) in items.iter().enumerate() {
        if matches!(
            file_mode(item, hash_key, local, patches, false),
            FileMode::Local | FileMode::Hybrid
        ) {
            tasks.push(InstallTask::Single(item.clone()));
            continue;
        }
        let Some(full) = hash_of_item(item, hash_key).and_then(|hash| ctx.find(&hash)) else {
            tasks.push(InstallTask::Single(item.clone()));
            continue;
        };
        if full.size == 0 {
            tasks.push(InstallTask::Single(item.clone()));
            continue;
        }
        let disk_hash = find_local(disk, &item.file_name).map(|f| f.hash.as_str());
        let patch = patches.iter().find(|p| {
            side_hash(&p.to, hash_key) == Some(full.name.as_str())
                && disk_hash.is_some()
                && side_hash(&p.from, hash_key) == disk_hash
        });
        let patch_range = patch.and_then(|p| {
            let name = format!("{}_{}", side_hash(&p.from, hash_key)?, full.name);
            let entry = ctx.find(&name)?;
            if entry.size == 0 {
                return None;
            }
            matching.insert(id, p.clone());
            Some(Range {
                start: entry.offset as u64,
                len: entry.size as u64,
            })
        });
        inputs.push(Input {
            id,
            raw_size: item.size,
            full: Range {
                start: full.offset as u64,
                len: full.size as u64,
            },
            patch: patch_range,
        });
    }
    for transfer in download_plan::plan(&inputs, ctx.policy) {
        if transfer.files.len() == 1 {
            tasks.push(InstallTask::Single(items[transfer.files[0].id].clone()));
        } else {
            let range = transfer.parts[0];
            tasks.push(InstallTask::Merged {
                files: transfer
                    .files
                    .into_iter()
                    .map(|file| FilePos {
                        item: items[file.id].clone(),
                        offset: file.range.start as usize,
                        size: file.range.len as usize,
                        patch: if file.patch {
                            matching.get(&file.id).cloned()
                        } else {
                            None
                        },
                    })
                    .collect(),
                range: range.key(),
                start: range.start as usize,
                download_size: range.len as usize,
            });
        }
    }
    tasks.sort_by_key(|task| {
        std::cmp::Reverse(match task {
            InstallTask::Single(item) => item.size,
            InstallTask::Merged { files, .. } => files.iter().map(|f| f.item.size).sum(),
        })
    });
    tasks
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
    item: &FileMeta,
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
        ranges.extend(
            ctx.policy
                .split(Range {
                    start: file.offset as u64,
                    len: file.size as u64,
                })
                .into_iter()
                .map(Range::key),
        );
    }
}

fn add_installer_range(ranges: &mut Vec<String>, item: &FileMeta, ctx: &SourceCtx) {
    if item.installer.unwrap_or(false) && ctx.installer_end > 0 {
        ranges.push(format!("0-{}", ctx.installer_end.saturating_sub(1)));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::plan::LocalFile;

    fn emb(name: &str, offset: usize, size: usize) -> Embedded {
        Embedded {
            name: name.to_string(),
            offset,
            raw_offset: 0,
            size,
        }
    }

    fn item(name: &str, hash: &str, installer: bool) -> FileMeta {
        FileMeta {
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
        ctx.restore_local_package(Some(&[emb("bbb", 100, 50), emb("aaa_bbb", 200, 21)]), None);
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
