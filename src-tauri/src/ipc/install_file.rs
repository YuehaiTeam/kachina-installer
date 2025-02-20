use crate::fs::{
    create_http_stream, create_local_stream, create_target_file, prepare_target, progressed_copy,
    progressed_hpatch, verify_hash,
};

fn default_as_false() -> bool {
    false
}

#[derive(serde::Deserialize, serde::Serialize, Clone, Debug)]
#[serde(untagged)]
enum InstallFileSource {
    Url {
        url: String,
        offset: usize,
        size: usize,
        #[serde(default = "default_as_false")]
        skip_decompress: bool,
    },
    Local {
        offset: usize,
        size: usize,
        #[serde(default = "default_as_false")]
        skip_decompress: bool,
    },
}

#[derive(serde::Deserialize, serde::Serialize, Clone, Debug)]
#[serde(tag = "type")]
enum InstallFileMode {
    Direct {
        source: InstallFileSource,
    },
    Patch {
        source: InstallFileSource,
        diff_size: usize,
    },
    HybridPatch {
        diff: InstallFileSource,
        source: InstallFileSource,
    },
}

#[derive(serde::Deserialize, serde::Serialize, Clone, Debug)]
pub struct InstallFileArgs {
    mode: InstallFileMode,
    target: String,
    md5: Option<String>,
    xxh: Option<String>,
}
async fn create_stream_by_source(
    source: InstallFileSource,
) -> Result<Box<dyn tokio::io::AsyncRead + Unpin + std::marker::Send>, String> {
    match source {
        InstallFileSource::Url {
            url,
            offset,
            size,
            skip_decompress,
        } => Ok(create_http_stream(&url, offset, size, skip_decompress)
            .await?
            .0),
        InstallFileSource::Local {
            offset,
            size,
            skip_decompress,
        } => Ok(create_local_stream(offset, size, skip_decompress).await?),
    }
}
pub async fn ipc_install_file(
    args: InstallFileArgs,
    notify: impl Fn(serde_json::Value) + std::marker::Send + 'static,
) -> Result<serde_json::Value, String> {
    let target = args.target;
    let override_old_path = prepare_target(&target).await?;
    let progress_noti = move |downloaded: usize| {
        notify(serde_json::json!(downloaded));
    };
    match args.mode {
        InstallFileMode::Direct { source } => {
            let res = progressed_copy(
                create_stream_by_source(source).await?,
                create_target_file(&target).await?,
                progress_noti,
            )
            .await?;
            verify_hash(&target, args.md5, args.xxh).await?;
            Ok(serde_json::json!(res))
        }
        InstallFileMode::Patch { source, diff_size } => {
            let res = progressed_hpatch(
                create_stream_by_source(source).await?,
                &target,
                diff_size,
                progress_noti,
                override_old_path,
            )
            .await?;
            verify_hash(&target, args.md5, args.xxh).await?;
            Ok(serde_json::json!(res))
        }
        InstallFileMode::HybridPatch { diff, source } => {
            // first extract source
            let source = create_stream_by_source(source).await?;
            let target_fs = create_target_file(&target).await?;
            progressed_copy(source, target_fs, progress_noti).await?;
            // then apply patch
            let size: usize = match diff {
                InstallFileSource::Url { size, .. } => size,
                InstallFileSource::Local { size, .. } => size,
            };
            progressed_hpatch(
                create_stream_by_source(diff).await?,
                &target,
                size,
                |_| {},
                None,
            )
            .await?;
            verify_hash(&target, args.md5, args.xxh).await?;
            Ok(serde_json::json!(()))
        }
    }
}
