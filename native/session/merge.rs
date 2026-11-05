use std::collections::HashMap;

use crate::pack_format::Embedded;
use crate::session::plan::{side_hash, DiskFiles, HashKey};
use crate::session::source::{hash_of_item, SourceCtx};
use crate::utils::metadata::{FileMeta, PatchInfo};

use super::download_plan::{self, Input, Range};

/// Where a single file's content comes from. [`plan_tasks`] decides it once;
/// the DFS2 range declaration and the executor read it. A diff that cannot be
/// resolved, and every retry, fetch the whole file instead.
#[derive(Debug, Clone)]
pub enum FileSource {
    /// A block embedded in this installer.
    Embedded(Embedded),
    /// A patch whose old file is an embedded block; the diff is fetched.
    Hybrid { base: Embedded, patch: PatchInfo },
    /// A patch from the file on disk; the diff is fetched.
    Patch(PatchInfo),
    /// The whole file is fetched.
    Whole,
}

#[derive(Debug, Clone)]
pub struct FilePlan {
    pub item: FileMeta,
    pub source: FileSource,
}

/// A member of a merged request: its slice of the response, and the patch
/// when the slice is a diff against the file on disk.
#[derive(Debug, Clone)]
pub struct FilePos {
    pub item: FileMeta,
    pub offset: usize,
    pub size: usize,
    pub patch: Option<PatchInfo>,
}

#[derive(Debug, Clone)]
pub enum InstallTask {
    Single(Box<FilePlan>),
    Merged {
        files: Vec<FilePos>,
        range: String,
        start: usize,
        download_size: usize,
    },
}

/// Embedded blocks by name and patches by the hash they produce, in the
/// order the lists give them; the first match wins.
struct Sources<'a> {
    hash_key: HashKey,
    embedded: HashMap<&'a str, &'a Embedded>,
    patches_to: HashMap<&'a str, Vec<&'a PatchInfo>>,
    disk: &'a DiskFiles,
}

impl<'a> Sources<'a> {
    fn new(
        hash_key: HashKey,
        embedded: &'a [Embedded],
        patches: &'a [PatchInfo],
        disk: &'a DiskFiles,
    ) -> Self {
        let mut by_name = HashMap::with_capacity(embedded.len());
        for block in embedded {
            by_name.entry(block.name.as_str()).or_insert(block);
        }
        let mut patches_to: HashMap<&str, Vec<&PatchInfo>> = HashMap::new();
        for patch in patches {
            if let Some(to) = side_hash(&patch.to, hash_key) {
                patches_to.entry(to).or_default().push(patch);
            }
        }
        Self {
            hash_key,
            embedded: by_name,
            patches_to,
            disk,
        }
    }

    /// In this order: an embedded block, a patch from an embedded block, a
    /// patch from the file on disk, the whole file.
    fn source(&self, item: &FileMeta) -> FileSource {
        let Some(hash) = hash_of_item(item, self.hash_key) else {
            return FileSource::Whole;
        };
        if let Some(block) = self.embedded.get(hash.as_str()) {
            return FileSource::Embedded((*block).clone());
        }
        let patches = self
            .patches_to
            .get(hash.as_str())
            .map(Vec::as_slice)
            .unwrap_or_default();
        let hybrid = patches.iter().find_map(|patch| {
            let base = self.embedded.get(side_hash(&patch.from, self.hash_key)?)?;
            Some(((*base).clone(), (*patch).clone()))
        });
        if let Some((base, patch)) = hybrid {
            return FileSource::Hybrid { base, patch };
        }
        let on_disk = self.disk.find(&item.file_name).map(|f| f.hash.as_str());
        let patch = on_disk.and_then(|disk| {
            patches
                .iter()
                .find(|patch| side_hash(&patch.from, self.hash_key) == Some(disk))
        });
        match patch {
            Some(patch) => FileSource::Patch((*patch).clone()),
            None => FileSource::Whole,
        }
    }
}

/// Name of the diff from `patch`'s old file to `hash` in a package index.
pub fn diff_name(patch: &PatchInfo, hash: &str, key: HashKey) -> Option<String> {
    Some(format!("{}_{hash}", side_hash(&patch.from, key)?))
}

pub fn plan_tasks(
    items: &[FileMeta],
    hash_key: HashKey,
    embedded: &[Embedded],
    patches: &[PatchInfo],
    ctx: &SourceCtx,
    disk: &DiskFiles,
) -> Vec<InstallTask> {
    let sources = Sources::new(hash_key, embedded, patches, disk);
    let mut inputs = Vec::new();
    let mut tasks = Vec::new();
    let mut matching = HashMap::new();
    for (id, item) in items.iter().enumerate() {
        let source = sources.source(item);
        let single = |source| {
            InstallTask::Single(Box::new(FilePlan {
                item: item.clone(),
                source,
            }))
        };
        if matches!(source, FileSource::Embedded(_) | FileSource::Hybrid { .. }) {
            tasks.push(single(source));
            continue;
        }
        let Some(full) = hash_of_item(item, hash_key).and_then(|hash| ctx.find(&hash)) else {
            tasks.push(single(source));
            continue;
        };
        if full.size == 0 {
            tasks.push(single(source));
            continue;
        }
        let patch_range = match &source {
            FileSource::Patch(patch) => diff_name(patch, &full.name, hash_key).and_then(|name| {
                let entry = ctx.find(&name).filter(|entry| entry.size > 0)?;
                matching.insert(id, patch.clone());
                Some(Range {
                    start: entry.offset as u64,
                    len: entry.size as u64,
                })
            }),
            _ => None,
        };
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
            let file = transfer.files[0];
            let source = match matching.get(&file.id) {
                Some(patch) if file.patch => FileSource::Patch(patch.clone()),
                _ => FileSource::Whole,
            };
            tasks.push(InstallTask::Single(Box::new(FilePlan {
                item: items[file.id].clone(),
                source,
            })));
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
            InstallTask::Single(plan) => plan.item.size,
            InstallTask::Merged { files, .. } => files.iter().map(|f| f.item.size).sum(),
        })
    });
    tasks
}

/// The ranges the executor can request for `tasks`: a single file's planned
/// diff and the whole file (sliced, and in one piece for retries); a merged
/// request's range and each member's whole file, which a failed member falls
/// back to. Embedded files declare nothing.
pub fn dfs2_ranges(tasks: &[InstallTask], ctx: &SourceCtx, hash_key: HashKey) -> Vec<String> {
    let mut ranges = Vec::new();
    for task in tasks {
        match task {
            InstallTask::Merged { range, files, .. } => {
                ranges.push(range.clone());
                for file in files {
                    add_whole_ranges(&mut ranges, &file.item, ctx, hash_key);
                }
            }
            InstallTask::Single(plan) => {
                let diff = match &plan.source {
                    FileSource::Embedded(_) => continue,
                    FileSource::Hybrid { patch, .. } | FileSource::Patch(patch) => Some(patch),
                    FileSource::Whole => None,
                };
                let hash = hash_of_item(&plan.item, hash_key);
                if let Some(name) = diff.zip(hash).and_then(|(p, h)| diff_name(p, &h, hash_key)) {
                    add_index_range(&mut ranges, ctx, &name);
                }
                add_whole_ranges(&mut ranges, &plan.item, ctx, hash_key);
            }
        }
    }
    ranges.sort();
    ranges.dedup();
    ranges
}

fn add_whole_ranges(ranges: &mut Vec<String>, item: &FileMeta, ctx: &SourceCtx, key: HashKey) {
    if let Some(hash) = hash_of_item(item, key) {
        add_index_range(ranges, ctx, &hash);
    }
    if item.installer.unwrap_or(false) && ctx.installer_end > 0 {
        ranges.push(format!("0-{}", ctx.installer_end.saturating_sub(1)));
    }
}

fn add_index_range(ranges: &mut Vec<String>, ctx: &SourceCtx, name: &str) {
    if let Some(file) = ctx.find(name) {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::plan::LocalFile;
    use crate::utils::metadata::PatchSide;

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

    fn on_disk(name: &str, hash: &str) -> LocalFile {
        LocalFile {
            file_name: name.to_string(),
            hash: hash.to_string(),
            size: 1,
            unwritable: false,
        }
    }

    fn kind(source: &FileSource) -> String {
        match source {
            FileSource::Embedded(block) => format!("embedded {}", block.name),
            FileSource::Hybrid { base, .. } => format!("hybrid from {}", base.name),
            FileSource::Patch(patch) => {
                format!("patch from {}", patch.from.md5.as_deref().unwrap())
            }
            FileSource::Whole => "whole".into(),
        }
    }

    #[test]
    fn source_prefers_embedded_then_hybrid_then_disk_patch() {
        let embedded = vec![emb("e1", 0, 10), emb("e2", 10, 10)];
        let patches = vec![
            patch("e2", "e1"),
            patch("d3", "h3"),
            patch("e2", "h2"),
            patch("d2", "h2"),
        ];
        let disk = DiskFiles::new(vec![on_disk("b.dll", "d2"), on_disk("c.dll", "d3")]);
        let sources = Sources::new(HashKey::Md5, &embedded, &patches, &disk);
        let kinds: Vec<String> = [
            item("a.dll", "e1", false),
            item("b.dll", "h2", false),
            item("c.dll", "h3", false),
            item("d.dll", "h4", false),
        ]
        .iter()
        .map(|i| kind(&sources.source(i)))
        .collect();
        assert_eq!(
            kinds,
            ["embedded e1", "hybrid from e2", "patch from d3", "whole"]
        );
    }

    #[test]
    fn skip_ranges_for_embedded_files() {
        let embedded = vec![emb("bbb", 0, 10)];
        let ctx = SourceCtx::from_embedded(&embedded);
        let disk = DiskFiles::new(vec![]);
        let tasks = plan_tasks(
            &[item("app.exe", "bbb", false)],
            HashKey::Md5,
            &embedded,
            &[],
            &ctx,
            &disk,
        );
        assert!(dfs2_ranges(&tasks, &ctx, HashKey::Md5).is_empty());
    }

    #[test]
    fn patch_declares_delta_and_full_file() {
        let mut ctx = SourceCtx::from_embedded(&[]);
        ctx.restore_local_package(Some(&[emb("bbb", 100, 50), emb("aaa_bbb", 200, 21)]), None);
        let disk = DiskFiles::new(vec![on_disk("app.exe", "aaa")]);
        let tasks = plan_tasks(
            &[item("app.exe", "bbb", false)],
            HashKey::Md5,
            &[],
            &[patch("aaa", "bbb")],
            &ctx,
            &disk,
        );
        let InstallTask::Single(plan) = &tasks[0] else {
            panic!("expected a single task")
        };
        assert_eq!(kind(&plan.source), "patch from aaa");
        assert_eq!(
            dfs2_ranges(&tasks, &ctx, HashKey::Md5),
            vec!["100-149".to_string(), "200-220".to_string()]
        );
    }

    #[test]
    fn merged_members_declare_only_their_whole_files() {
        let mut ctx = SourceCtx::from_embedded(&[]);
        ctx.restore_local_package(
            Some(&[
                emb("h1", 100, 10),
                emb("h2", 110, 10),
                emb("d1_h1", 120, 4),
                emb("d2_h2", 124, 4),
            ]),
            None,
        );
        let disk = DiskFiles::new(vec![on_disk("a.dll", "d1"), on_disk("b.dll", "d2")]);
        let tasks = plan_tasks(
            &[item("a.dll", "h1", false), item("b.dll", "h2", false)],
            HashKey::Md5,
            &[],
            &[patch("d1", "h1"), patch("d2", "h2")],
            &ctx,
            &disk,
        );
        let [InstallTask::Merged { range, files, .. }] = tasks.as_slice() else {
            panic!("expected one merged request, got {tasks:?}")
        };
        assert!(files.iter().all(|f| f.patch.is_some()));
        // a failed member is fetched whole, never by its diff alone
        assert_eq!(
            dfs2_ranges(&tasks, &ctx, HashKey::Md5),
            vec!["100-109".to_string(), "110-119".to_string(), range.clone()]
        );
    }

    #[test]
    fn installer_declares_prefix() {
        let mut ctx = SourceCtx::from_embedded(&[]);
        ctx.restore_local_package(Some(&[emb("upd", 80, 10)]), None);
        ctx.installer_end = 80;
        let disk = DiskFiles::new(vec![]);
        let tasks = plan_tasks(
            &[item("updater.exe", "upd", true)],
            HashKey::Md5,
            &[],
            &[],
            &ctx,
            &disk,
        );
        assert_eq!(
            dfs2_ranges(&tasks, &ctx, HashKey::Md5),
            vec!["0-79".to_string(), "80-89".to_string()]
        );
    }
}
