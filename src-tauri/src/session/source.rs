use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::anyhow;
use serde_json::Value;

use crate::dfs::{
    create_dfs2_session, end_dfs2_session, get_dfs, get_dfs2_batch_chunk_urls, get_dfs2_chunk_url,
    get_dfs2_metadata, get_http_with_range, solve_dfs2_challenge, Dfs2Data, Dfs2SessionInsights,
    InsightItem,
};
use crate::local::Embedded;
use crate::session::error::{self, hide, user};
use crate::session::plan::HashKey;
use crate::session::plugin::{
    clean_plugin_url, forced_plugin_name, is_github_source, resolve_github_file_url,
};
use crate::session::types::DfsMetadata;
use crate::session::ui::{PluginArgs, PluginHost, PluginResult};
use crate::utils::error::IntoAnyhow;
use crate::REQUEST_CLIENT;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RemoteKind {
    Direct,
    Dfs,
    Dfs2,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StorageKind {
    Packed,
    Hashed,
}

#[derive(Debug, Clone)]
pub enum ParsedSource {
    Http {
        remote: RemoteKind,
        storage: StorageKind,
        url: String,
    },
    Mirrorc {
        resource_id: String,
        channel: String,
        arch: Option<String>,
        os: Option<String>,
    },
    GitHub {
        raw: String,
        storage: StorageKind,
    },
    Plugin {
        name: String,
        raw: String,
    },
}

#[derive(Debug, Clone)]
pub struct FileLocation {
    pub url: Option<String>,
    pub offset: usize,
    pub size: usize,
    pub skip_decompress: bool,
}

#[derive(Debug, Clone)]
struct Dfs2Session {
    api_url: String,
    session_id: String,
    base_url: String,
    res_id: String,
}

#[derive(Default)]
pub struct SourceCtx {
    pub parsed: Option<ParsedSource>,
    pub index: HashMap<String, Embedded>,
    pub installer_end: usize,
    pub resource_version: Option<String>,
    dfs2: Option<Dfs2Session>,
    plugin_session: bool,
    plugin: Option<Arc<dyn PluginHost>>,
    insights: Mutex<Vec<InsightItem>>,
    chunk_urls: Mutex<HashMap<String, String>>,
}

impl SourceCtx {
    pub fn from_embedded(files: &[Embedded]) -> Self {
        let mut index = HashMap::new();
        for file in files {
            index.insert(file.name.clone(), file.clone());
        }
        Self {
            parsed: None,
            index,
            installer_end: 0,
            resource_version: None,
            dfs2: None,
            plugin_session: false,
            plugin: None,
            insights: Mutex::new(Vec::new()),
            chunk_urls: Mutex::new(HashMap::new()),
        }
    }

    pub fn attach_plugin(&mut self, host: Option<Arc<dyn PluginHost>>) {
        self.plugin = host;
    }

    pub fn find(&self, name: &str) -> Option<&Embedded> {
        self.index.get(name)
    }

    pub fn add_insight(&self, mut item: InsightItem, mode: &str) {
        item.mode = Some(mode.to_string());
        if let Ok(mut items) = self.insights.lock() {
            items.push(item);
        }
    }

    pub fn insight_snapshot(&self) -> Vec<InsightItem> {
        self.insights
            .lock()
            .map(|items| items.clone())
            .unwrap_or_default()
    }

    pub fn chunk_url(&self, range: &str) -> Option<String> {
        self.chunk_urls
            .lock()
            .ok()
            .and_then(|urls| urls.get(range).cloned())
    }

    pub fn put_chunk_urls(&self, urls: HashMap<String, String>) {
        if let Ok(mut cache) = self.chunk_urls.lock() {
            cache.extend(urls);
        }
    }

    pub fn restore_local_package(
        &mut self,
        embedded_index: Option<&[Embedded]>,
        resource_version: Option<String>,
    ) {
        self.index.clear();
        if let Ok(mut cache) = self.chunk_urls.lock() {
            cache.clear();
        }
        if let Some(index) = embedded_index {
            for file in index {
                self.index.insert(file.name.clone(), file.clone());
            }
            self.installer_end = index.iter().map(|file| file.offset).min().unwrap_or(0);
        } else {
            self.installer_end = 0;
        }
        self.resource_version = resource_version;
    }
}

pub fn parse_source(source: &str) -> anyhow::Result<ParsedSource> {
    if source.starts_with("mirrorc://") {
        let url = url::Url::parse(source)
            .map_err(|e| hide(format!("无法获取Mirror酱数据，安装包可能已经损坏：{source}"), e))?;
        let resource_id = url.host_str().unwrap_or_default().to_string();
        if resource_id.is_empty() {
            return Err(user(format!(
                "无法获取Mirror酱数据，安装包可能已经损坏：{source}"
            )));
        }
        let params = url.query_pairs();
        let mut channel = "stable".to_string();
        let mut arch = None;
        let mut os = None;
        for (k, v) in params {
            match k.as_ref() {
                "channel" => channel = v.into_owned(),
                "arch" => arch = Some(v.into_owned()),
                "os" => os = Some(v.into_owned()),
                _ => {}
            }
        }
        return Ok(ParsedSource::Mirrorc {
            resource_id,
            channel,
            arch,
            os,
        });
    }

    if let Some(name) = forced_plugin_name(source) {
        if name != "github" && !is_github_source(source) {
            return Ok(ParsedSource::Plugin {
                name,
                raw: source.to_string(),
            });
        }
    }

    let rest = source;
    let (remote, rest) = if let Some(r) = rest.strip_prefix("dfs2+") {
        (RemoteKind::Dfs2, r)
    } else if let Some(r) = rest.strip_prefix("dfs+") {
        (RemoteKind::Dfs, r)
    } else {
        (RemoteKind::Direct, rest)
    };
    let (storage_hint, rest) = if let Some(r) = rest.strip_prefix("hashed+") {
        (Some(StorageKind::Hashed), r)
    } else if let Some(r) = rest.strip_prefix("packed+") {
        (Some(StorageKind::Packed), r)
    } else if let Some(r) = rest.strip_prefix("auto+") {
        (None, r)
    } else {
        (None, rest)
    };
    let rest = if rest.contains("plugin-") {
        clean_plugin_url(rest)
    } else {
        rest.to_string()
    };
    if !rest.starts_with("http://") && !rest.starts_with("https://") {
        return Err(anyhow!("Invalid dfs source: {source}"));
    }
    let storage = if let Some(s) = storage_hint {
        s
    } else {
        let url = url::Url::parse(&rest).map_err(|_| anyhow!("Invalid dfs source: {source}"))?;
        if url.path().ends_with(".exe") {
            StorageKind::Packed
        } else if url.path().ends_with(".json") {
            StorageKind::Hashed
        } else {
            return Err(anyhow!("Invalid dfs source: {source}"));
        }
    };
    if is_github_source(source) || is_github_source(&rest) {
        return Ok(ParsedSource::GitHub {
            raw: source.to_string(),
            storage,
        });
    }
    Ok(ParsedSource::Http {
        remote,
        storage,
        url: rest,
    })
}

async fn fetch_hashed_metadata(url: &str) -> anyhow::Result<DfsMetadata> {
    let res = REQUEST_CLIENT
        .get(url)
        .send()
        .await
        .map_err(|e| hide(error::META_FAILED, e))?;
    if !res.status().is_success() {
        return Err(hide(
            error::META_FAILED,
            format!("hashed metadata http {}", res.status()),
        ));
    }
    let body = res
        .text()
        .await
        .map_err(|e| hide(error::META_FAILED, e))?;
    serde_json::from_str(&body).map_err(|e| hide(error::META_FAILED, e))
}

async fn fetch_dfs2_metadata(api_url: &str, ctx: &mut SourceCtx) -> anyhow::Result<DfsMetadata> {
    let dfs2 = get_dfs2_metadata(api_url.to_string())
        .await
        .map_err(|e| hide(error::META_FAILED, e))?;
    let data = dfs2
        .data
        .ok_or_else(|| hide(error::META_FAILED, "dfs2 metadata is null"))?;
    ctx.index.clear();
    for (name, info) in data.index {
        ctx.index.insert(
            name,
            Embedded {
                name: info.name,
                offset: info.offset as usize,
                raw_offset: info.raw_offset as usize,
                size: info.size as usize,
            },
        );
    }
    ctx.installer_end = data.installer_end as usize;
    ctx.resource_version = Some(dfs2.resource_version);
    serde_json::from_value(data.metadata).map_err(|e| hide(error::META_FAILED, e))
}

async fn refresh_packed_index(
    _source: &str,
    apiurl: &str,
    remote: RemoteKind,
    extras: Option<&str>,
    ctx: &mut SourceCtx,
) -> anyhow::Result<DfsMetadata> {
    let binurl = if remote == RemoteKind::Direct {
        apiurl.to_string()
    } else {
        resolve_dfs_file_url(apiurl, extras, Some(256), 0).await?
    };
    let (_status, pre) = get_http_with_range(binurl.clone(), 0, 256)
        .await
        .into_anyhow()?;
    let header_offset = find_subslice(&pre, b"!KachinaInstaller!")
        .ok_or_else(|| hide(error::META_FAILED, "invalid remote index header"))?;
    let index_offset = header_offset + 18;
    let index_start = read_u32be(&pre, index_offset)? as u64;
    let config_sz = read_u32be(&pre, index_offset + 4)? as u64;
    let theme_sz = read_u32be(&pre, index_offset + 8)? as u64;
    let index_sz = read_u32be(&pre, index_offset + 12)? as u64;
    let metadata_sz = read_u32be(&pre, index_offset + 16)? as u64;
    let data_end = index_start + index_sz + config_sz + theme_sz + metadata_sz;
    let (_status, index_data) = get_http_with_range(binurl, index_start, data_end - index_start)
        .await
        .into_anyhow()?;
    let mut metadata = None;
    let mut offset = 0usize;
    while offset + 4 <= index_data.len() {
        if &index_data[offset..offset + 4] != b"!IN\0" {
            offset += 1;
            continue;
        }
        offset += 4;
        if offset + 2 > index_data.len() {
            break;
        }
        let name_len = u16::from_be_bytes(index_data[offset..offset + 2].try_into().unwrap()) as usize;
        offset += 2;
        if offset + name_len + 4 > index_data.len() {
            break;
        }
        let name = String::from_utf8_lossy(&index_data[offset..offset + name_len]).to_string();
        offset += name_len;
        let size = read_u32be(&index_data, offset)? as usize;
        offset += 4;
        if offset + size > index_data.len() {
            break;
        }
        let data = &index_data[offset..offset + size];
        offset += size;
        match name.as_str() {
            "\0META" => {
                metadata = Some(
                    serde_json::from_slice::<DfsMetadata>(data)
                        .map_err(|e| hide(error::META_FAILED, e))?,
                );
            }
            "\0INDEX" => {
                parse_packed_index(data, index_start as usize, &mut ctx.index)?;
            }
            _ => {}
        }
    }
    ctx.installer_end = (index_start + config_sz + theme_sz) as usize;
    metadata.ok_or_else(|| hide(error::META_FAILED, "packed index has no metadata"))
}

fn parse_packed_index(
    data: &[u8],
    index_start: usize,
    out: &mut HashMap<String, Embedded>,
) -> anyhow::Result<()> {
    let mut idx = 0usize;
    while idx < data.len() {
        let name_len = *data.get(idx).ok_or_else(|| anyhow!("No index"))? as usize;
        idx += 1;
        if idx + name_len + 8 > data.len() {
            break;
        }
        let name = String::from_utf8_lossy(&data[idx..idx + name_len]).to_string();
        idx += name_len;
        let size = read_u32be(data, idx)? as usize;
        idx += 4;
        let rel = read_u32be(data, idx)? as usize;
        idx += 4;
        out.insert(
            name.clone(),
            Embedded {
                name,
                offset: index_start + rel,
                raw_offset: 0,
                size,
            },
        );
    }
    Ok(())
}

pub async fn fetch_metadata(
    source: &str,
    extras: Option<&str>,
    ctx: &mut SourceCtx,
) -> anyhow::Result<DfsMetadata> {
    let parsed = parse_source(source)?;
    ctx.parsed = Some(parsed.clone());
    match parsed {
        ParsedSource::Plugin { name, raw } => fetch_plugin_metadata(&name, &raw, extras, ctx).await,
        ParsedSource::Mirrorc { .. } => Err(hide(
            error::META_FAILED,
            "mirrorc metadata is handled separately",
        )),
        ParsedSource::GitHub { raw, storage } => match storage {
            StorageKind::Hashed => {
                let url = resolve_github_file_url(&raw).await?;
                fetch_hashed_metadata(&url).await
            }
            StorageKind::Packed => {
                let url = resolve_github_file_url(&raw).await?;
                refresh_packed_index(&raw, &url, RemoteKind::Direct, extras, ctx).await
            }
        },
        ParsedSource::Http {
            remote,
            storage,
            url,
        } => match remote {
            RemoteKind::Dfs2 => fetch_dfs2_metadata(&url, ctx).await,
            _ => match storage {
                StorageKind::Hashed => fetch_hashed_metadata(&url).await,
                StorageKind::Packed => refresh_packed_index(source, &url, remote, extras, ctx).await,
            },
        },
    }
}

pub async fn resolve_file_location(
    ctx: &SourceCtx,
    hash: &str,
    extras: Option<&str>,
    installer: bool,
) -> anyhow::Result<FileLocation> {
    let Some(parsed) = ctx.parsed.as_ref() else {
        return Err(hide(error::META_FAILED, "source not loaded"));
    };
    match parsed {
        ParsedSource::Plugin { name, raw } => {
            resolve_plugin_location(ctx, name, raw, hash, installer).await
        }
        ParsedSource::Mirrorc { .. } => Err(hide(error::META_FAILED, "mirrorc does not resolve hashed files")),
        ParsedSource::GitHub { raw, storage } => {
            let file_url = resolve_github_file_url(raw).await?;
            match storage {
                StorageKind::Hashed => Ok(FileLocation {
                    url: Some(hashed_file_url(&file_url, hash)),
                    offset: 0,
                    size: 0,
                    skip_decompress: false,
                }),
                StorageKind::Packed => {
                    if let Some(file) = ctx.find(hash) {
                        Ok(FileLocation {
                            url: Some(file_url),
                            offset: file.offset,
                            size: file.size,
                            skip_decompress: false,
                        })
                    } else if installer {
                        Ok(FileLocation {
                            url: Some(file_url),
                            offset: 0,
                            size: ctx.installer_end,
                            skip_decompress: true,
                        })
                    } else {
                        Err(hide(error::FILE_MISSING, "no file in remote binary"))
                    }
                }
            }
        }
        ParsedSource::Http {
            remote,
            storage,
            url,
        } => match remote {
            RemoteKind::Dfs2 => resolve_dfs2_location(ctx, hash, installer).await,
            _ => match storage {
                StorageKind::Hashed => {
                    let file_url = if *remote == RemoteKind::Direct {
                        hashed_file_url(url, hash)
                    } else {
                        resolve_dfs_file_url(&hashed_file_url(url, hash), extras, None, 0).await?
                    };
                    Ok(FileLocation {
                        url: Some(file_url),
                        offset: 0,
                        size: 0,
                        skip_decompress: false,
                    })
                }
                StorageKind::Packed => {
                    if let Some(file) = ctx.find(hash) {
                        let full = if *remote == RemoteKind::Direct {
                            url.clone()
                        } else {
                            resolve_dfs_file_url(url, extras, Some(file.size), file.offset).await?
                        };
                        Ok(FileLocation {
                            url: Some(full),
                            offset: file.offset,
                            size: file.size,
                            skip_decompress: false,
                        })
                    } else if installer {
                        let full = if *remote == RemoteKind::Direct {
                            url.clone()
                        } else {
                            resolve_dfs_file_url(url, extras, Some(ctx.installer_end), 0).await?
                        };
                        Ok(FileLocation {
                            url: Some(full),
                            offset: 0,
                            size: ctx.installer_end,
                            skip_decompress: true,
                        })
                    } else {
                        Err(hide(error::FILE_MISSING, "no file in remote binary"))
                    }
                }
            },
        },
    }
}

async fn resolve_dfs2_location(
    ctx: &SourceCtx,
    hash: &str,
    installer: bool,
) -> anyhow::Result<FileLocation> {
    let session = ctx
        .dfs2
        .as_ref()
        .ok_or_else(|| hide(error::DFS2_SESSION, "DFS2 session not found"))?;
    let session_api = format!(
        "{}/session/{}/{}",
        session.base_url, session.session_id, session.res_id
    );
    if let Some(file) = ctx.find(hash) {
        let range = format!("{}-{}", file.offset, file.offset + file.size.saturating_sub(1));
        let url = dfs2_chunk_url(ctx, &session_api, &range).await?;
        Ok(FileLocation {
            url: Some(url),
            offset: file.offset,
            size: file.size,
            skip_decompress: false,
        })
    } else if installer {
        let end = ctx.installer_end.max(1);
        let range = format!("0-{}", end - 1);
        let url = dfs2_chunk_url(ctx, &session_api, &range).await?;
        Ok(FileLocation {
            url: Some(url),
            offset: 0,
            size: ctx.installer_end,
            skip_decompress: true,
        })
    } else {
        Err(hide(error::FILE_MISSING, "no file in dfs2 index"))
    }
}

async fn dfs2_chunk_url(ctx: &SourceCtx, session_api: &str, range: &str) -> anyhow::Result<String> {
    if let Some(url) = ctx.chunk_url(range) {
        return Ok(url);
    }
    let resp = get_dfs2_chunk_url(session_api.to_string(), range.to_string())
        .await
        .map_err(|e| anyhow!(e))?;
    Ok(resp.url)
}

pub async fn resolve_range_url(
    ctx: &SourceCtx,
    extras: Option<&str>,
    start: usize,
    size: usize,
) -> anyhow::Result<String> {
    let Some(parsed) = ctx.parsed.as_ref() else {
        return Err(hide(error::META_FAILED, "source not loaded"));
    };
    match parsed {
        ParsedSource::Plugin { name, raw } => {
            let end = start + size.saturating_sub(1);
            plugin_chunk_url(ctx, name, raw, &format!("{start}-{end}")).await
        }
        ParsedSource::Mirrorc { .. } => Err(hide(error::META_FAILED, "mirrorc has no range url")),
        ParsedSource::GitHub { raw, .. } => resolve_github_file_url(raw).await,
        ParsedSource::Http {
            remote,
            url,
            ..
        } => match remote {
            RemoteKind::Direct => Ok(url.clone()),
            RemoteKind::Dfs => resolve_dfs_file_url(url, extras, Some(size), start).await,
            RemoteKind::Dfs2 => {
                let session = ctx
                    .dfs2
                    .as_ref()
                    .ok_or_else(|| hide(error::DFS2_SESSION, "DFS2 session not found"))?;
                let session_api = format!(
                    "{}/session/{}/{}",
                    session.base_url, session.session_id, session.res_id
                );
                let end = start + size.saturating_sub(1);
                let range = format!("{start}-{end}");
                dfs2_chunk_url(ctx, &session_api, &range)
                    .await
                    .map_err(|e| hide(error::DFS2_SESSION, e))
            }
        },
    }
}

pub async fn ensure_dfs2_session(
    ctx: &mut SourceCtx,
    ranges: Vec<String>,
    extras: Option<&str>,
) -> anyhow::Result<()> {
    if let Some(ParsedSource::Plugin { name, raw }) = ctx.parsed.clone() {
        return ensure_plugin_session(ctx, &name, &raw, ranges).await;
    }
    let Some(ParsedSource::Http {
        remote: RemoteKind::Dfs2,
        url,
        ..
    }) = ctx.parsed.clone()
    else {
        return Ok(());
    };
    if ranges.is_empty() {
        return Ok(());
    }
    let extras_obj = extras.filter(|s| !s.trim().is_empty()).and_then(|s| {
        match serde_json::from_str::<Value>(s) {
            Ok(value) => Some(value),
            Err(err) => {
                tracing::warn!("dfs extras is not JSON, ignore: {err}");
                None
            }
        }
    });
    let sid = create_dfs2_session_with_challenge(
        url.clone(),
        Some(ranges),
        ctx.resource_version.clone(),
        extras_obj,
    )
    .await?;
    let parsed = url::Url::parse(&url).map_err(|e| hide(error::DFS2_SESSION, e))?;
    let base_url = format!("{}://{}", parsed.scheme(), parsed.host_str().unwrap_or(""));
    let res_id = parsed
        .path_segments()
        .and_then(|s| s.last())
        .unwrap_or("")
        .to_string();
    ctx.dfs2 = Some(Dfs2Session {
        api_url: url,
        session_id: sid,
        base_url,
        res_id,
    });
    Ok(())
}

fn is_network_err(err: &impl std::fmt::Display) -> bool {
    let s = err.to_string();
    if s.contains("Session creation failed") {
        return false;
    }
    let check = s.to_lowercase();
    check.contains("failed to send request")
        || check.contains("connection reset")
        || check.contains("connection refused")
        || check.contains("timed out")
        || check.contains("timeout")
        || check.contains("10054")
        || check.contains("10060")
        || check.contains("10061")
}

async fn create_dfs2_session_with_challenge(
    api_url: String,
    chunks: Option<Vec<String>>,
    version: Option<String>,
    extras: Option<Value>,
) -> anyhow::Result<String> {
    let delays = [200u64, 600, 1000];
    let mut last_err = None;
    for (i, delay) in delays.iter().enumerate() {
        match create_dfs2_session_once(
            api_url.clone(),
            chunks.clone(),
            version.clone(),
            extras.clone(),
        )
        .await
        {
            Ok(sid) => return Ok(sid),
            Err(err) => {
                if is_network_err(&err) && i + 1 < delays.len() {
                    tracing::warn!("dfs2 session network error, retry in {delay}ms: {err:#}");
                    tokio::time::sleep(Duration::from_millis(*delay)).await;
                    last_err = Some(err);
                    continue;
                }
                return Err(err);
            }
        }
    }
    Err(last_err.unwrap_or_else(|| anyhow!(error::DFS2_SESSION)))
}

async fn create_dfs2_session_once(
    api_url: String,
    chunks: Option<Vec<String>>,
    version: Option<String>,
    extras: Option<Value>,
) -> anyhow::Result<String> {
    let mut challenge_response = None;
    let mut session_id = None;
    for _ in 0..3 {
        let resp = create_dfs2_session(
            api_url.clone(),
            chunks.clone(),
            version.clone(),
            challenge_response.clone(),
            session_id.clone(),
            extras.clone(),
        )
        .await
        .map_err(|e| anyhow!(e))?;
        if let Some(sid) = resp.sid.clone() {
            if resp.challenge.is_none() {
                return Ok(sid);
            }
        }
        if let (Some(challenge), Some(data), Some(sid)) =
            (resp.challenge, resp.data, resp.sid)
        {
            if challenge == "web" {
                let _ = data;
                return Err(user("当前下载源需要网页验证，请更换下载源"));
            }
            challenge_response = Some(
                solve_dfs2_challenge(challenge, data)
                    .await
                    .map_err(|e| anyhow!(e))?,
            );
            session_id = Some(sid);
            continue;
        }
        return Err(hide(error::DFS2_SESSION, "invalid session response format"));
    }
    Err(hide(
        error::DFS2_SESSION,
        "failed to create session after 3 challenge attempts",
    ))
}

pub async fn cleanup_dfs2(ctx: &mut SourceCtx) {
    let insights = ctx.insight_snapshot();
    let payload = if insights.is_empty() {
        None
    } else {
        Some(Dfs2SessionInsights {
            servers: insights.clone(),
        })
    };
    if let Some(session) = ctx.dfs2.take() {
        let session_api = format!(
            "{}/session/{}/{}",
            session.base_url, session.session_id, session.res_id
        );
        if let Err(err) = end_dfs2_session(session_api, payload).await {
            tracing::warn!("end dfs2 session failed: {err}");
        }
        let _ = session.api_url;
    }
    if ctx.plugin_session {
        ctx.plugin_session = false;
        if let Some(ParsedSource::Plugin { name, raw }) = ctx.parsed.clone() {
            let insights = if insights.is_empty() {
                None
            } else {
                Some(serde_json::json!({ "servers": insights }))
            };
            match plugin_call(
                ctx,
                PluginArgs {
                    method: "endSession".to_string(),
                    name,
                    url: clean_plugin_url(&raw),
                    range: None,
                    diffchunks: None,
                    insights,
                },
            )
            .await
            {
                Ok(PluginResult::Unimplemented) | Ok(PluginResult::Value(_)) => {}
                Err(err) => tracing::warn!("plugin endSession failed: {err}"),
            }
        }
    }
}

async fn fetch_plugin_metadata(
    name: &str,
    raw: &str,
    extras: Option<&str>,
    ctx: &mut SourceCtx,
) -> anyhow::Result<DfsMetadata> {
    let clean = clean_plugin_url(raw);
    match plugin_call(
        ctx,
        PluginArgs {
            method: "getMetadata".to_string(),
            name: name.to_string(),
            url: clean.clone(),
            range: None,
            diffchunks: None,
            insights: None,
        },
    )
    .await?
    {
        PluginResult::Value(data) if !data.is_null() => apply_plugin_metadata(ctx, data),
        PluginResult::Unimplemented | PluginResult::Value(_) => {
            let url = plugin_chunk_url(ctx, name, raw, "").await?;
            refresh_packed_index(raw, &url, RemoteKind::Direct, extras, ctx).await
        }
    }
}

fn apply_plugin_metadata(ctx: &mut SourceCtx, data: Value) -> anyhow::Result<DfsMetadata> {
    let data: Dfs2Data =
        serde_json::from_value(data).map_err(|e| hide(error::META_FAILED, e))?;
    ctx.index.clear();
    for (name, info) in data.index {
        ctx.index.insert(
            name,
            Embedded {
                name: info.name,
                offset: info.offset as usize,
                raw_offset: info.raw_offset as usize,
                size: info.size as usize,
            },
        );
    }
    ctx.installer_end = data.installer_end as usize;
    serde_json::from_value(data.metadata).map_err(|e| hide(error::META_FAILED, e))
}

async fn resolve_plugin_location(
    ctx: &SourceCtx,
    name: &str,
    raw: &str,
    hash: &str,
    installer: bool,
) -> anyhow::Result<FileLocation> {
    if let Some(file) = ctx.find(hash) {
        let end = file.offset + file.size.saturating_sub(1);
        let url = plugin_chunk_url(ctx, name, raw, &format!("{}-{end}", file.offset)).await?;
        Ok(FileLocation {
            url: Some(url),
            offset: file.offset,
            size: file.size,
            skip_decompress: false,
        })
    } else if installer {
        let end = ctx.installer_end.max(1);
        let url = plugin_chunk_url(ctx, name, raw, &format!("0-{}", end - 1)).await?;
        Ok(FileLocation {
            url: Some(url),
            offset: 0,
            size: ctx.installer_end,
            skip_decompress: true,
        })
    } else {
        Err(hide(error::FILE_MISSING, "no file in remote binary"))
    }
}

async fn ensure_plugin_session(
    ctx: &mut SourceCtx,
    name: &str,
    raw: &str,
    ranges: Vec<String>,
) -> anyhow::Result<()> {
    if ranges.is_empty() || ctx.plugin_session {
        return Ok(());
    }
    match plugin_call(
        ctx,
        PluginArgs {
            method: "createSession".to_string(),
            name: name.to_string(),
            url: clean_plugin_url(raw),
            range: None,
            diffchunks: Some(ranges),
            insights: None,
        },
    )
    .await
    {
        Ok(PluginResult::Unimplemented) => Ok(()),
        Ok(PluginResult::Value(_)) => {
            ctx.plugin_session = true;
            Ok(())
        }
        Err(err) => Err(hide(error::DFS2_SESSION, err)),
    }
}

async fn plugin_chunk_url(
    ctx: &SourceCtx,
    name: &str,
    raw: &str,
    range: &str,
) -> anyhow::Result<String> {
    match plugin_call(
        ctx,
        PluginArgs {
            method: "getChunkUrl".to_string(),
            name: name.to_string(),
            url: clean_plugin_url(raw),
            range: Some(range.to_string()),
            diffchunks: None,
            insights: None,
        },
    )
    .await?
    {
        PluginResult::Unimplemented => Err(error::plugin_not_found(name)),
        PluginResult::Value(data) => data
            .get("url")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .map(|s| s.to_string())
            .ok_or_else(|| hide(error::NO_DOWNLOAD_NODE, "plugin getChunkUrl returned no url")),
    }
}

async fn plugin_call(ctx: &SourceCtx, args: PluginArgs) -> anyhow::Result<PluginResult> {
    let Some(host) = ctx.plugin.as_ref() else {
        return Err(user(error::PLUGIN_NO_UI));
    };
    host.call(args).await
}

pub fn hash_of_item(item: &crate::session::plan::HashInfo, key: HashKey) -> Option<String> {
    match key {
        HashKey::Md5 => item.md5.clone(),
        HashKey::Xxh => item.xxh.clone(),
    }
}

fn hashed_file_url(json_url: &str, hash: &str) -> String {
    let url = url::Url::parse(json_url).ok();
    if let Some(url) = url {
        let path = url.path();
        let dir = path.rsplit_once('/').map(|(d, _)| d).unwrap_or("");
        return format!("{}{dir}/hashed/{hash}", {
            let mut origin = format!("{}://{}", url.scheme(), url.host_str().unwrap_or(""));
            if let Some(port) = url.port() {
                origin.push_str(&format!(":{port}"));
            }
            origin
        });
    }
    json_url.to_string()
}

async fn resolve_dfs_file_url(
    apiurl: &str,
    extras: Option<&str>,
    length: Option<usize>,
    start: usize,
) -> anyhow::Result<String> {
    let range = length.map(|len| format!("{}-{}", start, start + len.saturating_sub(1)));
    let dfs = get_dfs(
        apiurl.to_string(),
        range,
        extras.map(|s| s.to_string()),
    )
    .await
    .map_err(|e| anyhow!(e))?;
    if let Some(url) = dfs.url {
        return Ok(url);
    }
    if let Some(tests) = dfs.tests {
        if let Some(url) = pick_fastest_test(tests).await {
            return Ok(url);
        }
    }
    if let Some(source) = dfs.source {
        return Ok(source);
    }
    Err(user(error::NO_DOWNLOAD_NODE))
}

async fn pick_fastest_test(tests: Vec<(String, String)>) -> Option<String> {
    if tests.is_empty() {
        return None;
    }
    let mut joins = tokio::task::JoinSet::new();
    for (probe, url) in tests.clone() {
        joins.spawn(async move {
            let client = reqwest::Client::builder()
                .timeout(Duration::from_millis(2000))
                .build()
                .ok()?;
            let ok = client.head(&probe).send().await.ok()?.status().is_success();
            if ok {
                Some(url)
            } else {
                None
            }
        });
    }
    while let Some(res) = joins.join_next().await {
        if let Ok(Some(url)) = res {
            return Some(url);
        }
    }
    tests.into_iter().next().map(|t| t.1)
}

fn find_subslice(hay: &[u8], needle: &[u8]) -> Option<usize> {
    hay.windows(needle.len()).position(|w| w == needle)
}

fn read_u32be(data: &[u8], offset: usize) -> anyhow::Result<u32> {
    let bytes: [u8; 4] = data
        .get(offset..offset + 4)
        .ok_or_else(|| hide(error::META_FAILED, "invalid remote index"))?
        .try_into()
        .unwrap();
    Ok(u32::from_be_bytes(bytes))
}

pub async fn prefetch_chunk_urls(ctx: &SourceCtx, ranges: Vec<String>) {
    if ranges.is_empty() || ctx.dfs2.is_none() {
        return;
    }
    match prefetch_batch_urls(ctx, ranges).await {
        Ok(urls) => ctx.put_chunk_urls(urls),
        Err(err) => tracing::warn!("prefetch chunk urls failed: {err:#}"),
    }
}

async fn prefetch_batch_urls(
    ctx: &SourceCtx,
    ranges: Vec<String>,
) -> anyhow::Result<HashMap<String, String>> {
    let Some(session) = ctx.dfs2.as_ref() else {
        return Ok(HashMap::new());
    };
    let session_api = format!(
        "{}/session/{}/{}",
        session.base_url, session.session_id, session.res_id
    );
    let resp = get_dfs2_batch_chunk_urls(session_api, ranges)
        .await
        .map_err(|e| anyhow!(e))?;
    let mut out = HashMap::new();
    for (k, v) in resp.urls {
        if let Some(url) = v.url {
            out.insert(k, url);
        }
    }
    Ok(out)
}
