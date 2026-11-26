#[path = "../../../native/cli/arg.rs"]
pub mod arg;

use std::path::PathBuf;

use clap::{Parser, Subcommand};

#[derive(Debug, Clone, clap::Args)]
pub struct PackArgs {
    #[clap(long, short = 'o', default_value = "output.exe")]
    pub output: PathBuf,
    #[clap(long, short = 'c', default_value = ".config.json")]
    pub config: PathBuf,
    #[clap(long, short = 't')]
    pub image: Option<PathBuf>,
    #[clap(long, short = 'm')]
    pub metadata: Option<PathBuf>,
    #[clap(long, short = 'd')]
    pub data_dir: Option<PathBuf>,
    #[clap(long)]
    pub icon: Option<PathBuf>,
    /// 裸安装器 exe 路径；缺省从自身拼接的 bundle 提取，只有未拼接的裸 builder 才需要显式指定
    #[clap(long, short = 'b')]
    pub base: Option<PathBuf>,
}

#[derive(Debug, Clone, clap::Args)]
pub struct GenArgs {
    #[clap(long, short = 'i')]
    pub input_dir: PathBuf,
    #[clap(long, short = 'm')]
    pub output_metadata: PathBuf,
    #[clap(long, short = 'o')]
    pub output_dir: PathBuf,
    #[clap(long, short = 'r')]
    pub repo: String,
    #[clap(long, short = 't')]
    pub tag: String,
    #[clap(long, short = 'd')]
    pub diff_vers: Option<Vec<String>>,
    #[clap(long, short = 'x')]
    pub diff_ignore: Option<Vec<String>>,
    #[clap(long, short = 'u')]
    pub updater: Option<PathBuf>,
    #[clap(long, short = 'p')]
    pub updater_name: Option<String>,
    #[clap(long, short = 'j', default_value = "2")]
    pub zstd_concurrency: usize,
}

#[derive(Debug, Clone, clap::Args)]
pub struct AppendArgs {
    #[clap(long, short = 'o', default_value = "output.exe")]
    pub output: PathBuf,
    #[clap(long, short = 'f')]
    pub file: Vec<PathBuf>,
    #[clap(long, short = 'n')]
    pub name: Vec<String>,
}

#[derive(Debug, Clone, clap::Args)]
pub struct ExtractArgs {
    #[clap(long, short = 'i', default_value = "output.exe")]
    pub input: PathBuf,

    // 原有参数保持不变
    #[clap(long, short = 'f')]
    pub file: Vec<PathBuf>,
    #[clap(long, short = 'n')]
    pub name: Vec<String>,

    // 新增参数
    #[clap(long)]
    pub meta_name: Vec<String>,
    #[clap(long)]
    pub all: Option<PathBuf>,
    #[clap(long)]
    pub list: bool,
}

#[derive(Debug, Clone, clap::Args)]
pub struct ReplaceBinArgs {
    /// 输入的安装包文件
    pub input: PathBuf,
    /// 输出的新安装包文件
    #[clap(long, short = 'o')]
    pub output: PathBuf,
    /// 显式指定新 base 程序；缺省取 bundle 末位 PE
    #[clap(long)]
    pub base: Option<PathBuf>,
    /// 写入新 base 的图标；应用名称与窗口标题取自包内配置
    #[clap(long)]
    pub icon: Option<PathBuf>,
}

#[derive(Debug, Clone, clap::Args)]
pub struct SplitArgs {
    /// 输入的最终成品安装包（签名后执行拆包，保证头部字节与完整包源一致）
    #[clap(long, short = 'i')]
    pub input: PathBuf,
    /// 输出目录：写入 cas/{xxh} 块与 <name>.json
    #[clap(long, short = 'o')]
    pub output: PathBuf,
    /// {ver}.json 的文件名（不带扩展名）；缺省取输入文件名
    #[clap(long, short = 'n')]
    pub name: Option<String>,
}

#[derive(Subcommand, Clone, Debug)]
pub enum Command {
    Pack(PackArgs),
    Append(AppendArgs),
    Extract(ExtractArgs),
    Gen(GenArgs),
    ReplaceBin(ReplaceBinArgs),
    Split(SplitArgs),
}

#[derive(Parser)]
#[command(args_conflicts_with_subcommands = true, arg_required_else_help = true)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Option<Command>,
}
