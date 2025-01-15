use crate::fs::{
    create_http_stream, create_local_stream, create_target_file, prepare_target, progressed_copy,
    progressed_hpatch,
};
#[derive(serde::Deserialize, serde::Serialize, Clone, Debug)]
#[serde(untagged)]
enum InstallFileSource {
    Url(String),
    Local { offset: usize, size: usize },
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
        diff_url: String,
        diff_size: usize,
        source_offset: usize,
        source_size: usize,
    },
}

#[derive(serde::Deserialize, serde::Serialize, Clone, Debug)]
pub struct InstallFileArgs {
    mode: InstallFileMode,
    target: String,
}
async fn create_stream_by_source(
    source: InstallFileSource,
) -> Result<Box<dyn tokio::io::AsyncRead + Unpin + std::marker::Send>, String> {
    match source {
        InstallFileSource::Url(url) => Ok(Box::new(create_http_stream(&url).await?)),
        InstallFileSource::Local { offset, size } => {
            Ok(Box::new(create_local_stream(offset, size).await?))
        }
    }
}
pub async fn ipc_install_file(
    args: InstallFileArgs,
    notify: impl Fn(serde_json::Value) + std::marker::Send + 'static,
) -> Result<serde_json::Value, String> {
    let target = args.target;
    prepare_target(&target).await?;
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
            Ok(serde_json::json!(res))
        }
        InstallFileMode::Patch { source, diff_size } => {
            let res = progressed_hpatch(
                create_stream_by_source(source).await?,
                &target,
                diff_size,
                progress_noti,
            )
            .await?;
            Ok(serde_json::json!(res))
        }
        InstallFileMode::HybridPatch {
            diff_url,
            diff_size,
            source_offset,
            source_size,
        } => {
            // first extract source
            let source = create_local_stream(source_offset, source_size).await?;
            let target_fs = create_target_file(&target).await?;
            progressed_copy(source, target_fs, progress_noti).await?;
            // then apply patch
            progressed_hpatch(
                create_http_stream(&diff_url).await?,
                &target,
                diff_size,
                |_| {},
            )
            .await?;
            Ok(serde_json::json!(()))
        }
    }
}
