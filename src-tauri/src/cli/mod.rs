pub mod arg;

use std::ffi::OsString;
use std::path::PathBuf;

use arg::{Command, InstallArgs, UacArgs};

/// 解析永不失败：未知 token（flag、位置参数、slash 开关）一律跳过，
/// 已识别的选项照常生效，最坏情况即默认交互安装。
pub fn parse() -> Command {
    let argv: Vec<OsString> = std::env::args_os().skip(1).collect();
    parse_from(&argv, raw_command_line_d_tail())
}

struct OptSpec {
    short: Option<char>,
    long: Option<&'static str>,
    takes_value: bool,
    hidden: bool,
    value_name: &'static str,
    help: &'static str,
    apply: fn(&mut InstallArgs, Option<OsString>),
}

const OPTS: &[OptSpec] = &[
    OptSpec {
        short: Some('D'),
        long: None,
        takes_value: true,
        hidden: false,
        value_name: "DIR",
        help: "Install directory",
        apply: |a, v| a.target = v.map(PathBuf::from),
    },
    OptSpec {
        short: Some('I'),
        long: None,
        takes_value: false,
        hidden: false,
        value_name: "",
        help: "Non-interactive install",
        apply: |a, _| a.non_interactive = true,
    },
    OptSpec {
        short: Some('S'),
        long: None,
        takes_value: false,
        hidden: false,
        value_name: "",
        help: "Silent install",
        apply: |a, _| a.silent = true,
    },
    OptSpec {
        short: Some('O'),
        long: None,
        takes_value: false,
        hidden: false,
        value_name: "",
        help: "Force online install",
        apply: |a, _| a.online = true,
    },
    OptSpec {
        short: Some('U'),
        long: None,
        takes_value: false,
        hidden: false,
        value_name: "",
        help: "Uninstall",
        apply: |a, _| a.uninstall = true,
    },
    OptSpec {
        short: None,
        long: Some("source"),
        takes_value: true,
        hidden: true,
        value_name: "ID",
        help: "Override install source",
        apply: |a, v| a.source = v.map(lossy),
    },
    OptSpec {
        short: None,
        long: Some("dfs-extras"),
        takes_value: true,
        hidden: true,
        value_name: "JSON",
        help: "DFS extra data",
        apply: |a, v| a.dfs_extras = v.map(lossy),
    },
    OptSpec {
        short: None,
        long: Some("mirrorc-cdk"),
        takes_value: true,
        hidden: true,
        value_name: "CDK",
        help: "Override MirrorChyan CDK",
        apply: |a, v| a.mirrorc_cdk = v.map(lossy),
    },
    OptSpec {
        short: None,
        long: Some("dump-dir"),
        takes_value: true,
        hidden: true,
        value_name: "DIR",
        help: "Write session dump JSON here (dev / tests only)",
        apply: |a, v| a.dump_dir = v.map(PathBuf::from),
    },
];

fn lossy(v: OsString) -> String {
    v.to_string_lossy().into_owned()
}

fn parse_from(argv: &[OsString], d_tail: Option<PathBuf>) -> Command {
    match argv.first().and_then(|s| s.to_str()) {
        Some("install") => Command::Install(parse_install(&argv[1..], d_tail)),
        Some("native-ui") => Command::NativeUi(parse_install(&argv[1..], d_tail)),
        Some("install-webview2") => Command::InstallWebview2,
        Some("headless-uac") => Command::HeadlessUac(parse_uac(&argv[1..])),
        _ => Command::Install(parse_install(argv, d_tail)),
    }
}

fn parse_uac(argv: &[OsString]) -> UacArgs {
    // 仅由自身以正确参数拉起；缺参属内部误用，静默退出。
    let Some(pipe_id) = argv.first().and_then(|s| s.to_str()) else {
        std::process::exit(2);
    };
    UacArgs {
        pipe_id: pipe_id.to_string(),
    }
}

fn parse_install(argv: &[OsString], d_tail: Option<PathBuf>) -> InstallArgs {
    let mut args = InstallArgs::default();
    let mut slash_target: Option<PathBuf> = None;
    let mut i = 0;
    while i < argv.len() {
        let Some(tok) = argv[i].to_str() else {
            // 非 UTF-8 token 只可能是值；作为独立 token 出现时跳过
            i += 1;
            continue;
        };

        if tok == "-h" || tok == "--help" || tok == "/?" {
            help_and_exit();
        }

        if let Some(rest) = tok.strip_prefix('/') {
            apply_slash(rest, &mut args, &mut slash_target);
            i += 1;
            continue;
        }

        if let Some(long) = tok.strip_prefix("--") {
            let (name, inline) = match long.split_once('=') {
                Some((n, v)) => (n, Some(OsString::from(v))),
                None => (long, None),
            };
            if let Some(spec) = OPTS.iter().find(|o| o.long == Some(name)) {
                if spec.takes_value {
                    let value = inline.or_else(|| next_value(argv, &mut i));
                    if value.is_some() {
                        (spec.apply)(&mut args, value);
                    }
                } else {
                    (spec.apply)(&mut args, None);
                }
            }
            i += 1;
            continue;
        }

        if let Some(shorts) = tok.strip_prefix('-') {
            let mut chars = shorts.chars();
            while let Some(c) = chars.next() {
                let Some(spec) = OPTS.iter().find(|o| o.short == Some(c)) else {
                    continue; // 未知短旗标：跳过该字符
                };
                if !spec.takes_value {
                    (spec.apply)(&mut args, None);
                    continue;
                }
                // 取值短旗标：token 剩余部分（允许 -Dfoo / -D=foo）或下一个 token
                let rest: String = chars.collect();
                let rest = rest.strip_prefix('=').map(str::to_string).unwrap_or(rest);
                let value = if !rest.is_empty() {
                    Some(OsString::from(rest))
                } else {
                    next_value(argv, &mut i)
                };
                if value.is_some() {
                    (spec.apply)(&mut args, value);
                }
                break;
            }
            i += 1;
            continue;
        }

        // 裸位置参数（拖拽文件等）：跳过
        i += 1;
    }

    // target 优先级：显式 -D > 原始命令行 /D= 尾部 > /DIR= token
    if args.target.is_none() {
        args.target = d_tail.or(slash_target);
    }
    args
}

fn next_value(argv: &[OsString], i: &mut usize) -> Option<OsString> {
    if *i + 1 < argv.len() {
        *i += 1;
        Some(argv[*i].clone())
    } else {
        None
    }
}

fn apply_slash(rest: &str, args: &mut InstallArgs, slash_target: &mut Option<PathBuf>) {
    let upper = rest.to_ascii_uppercase();
    match upper.as_str() {
        "S" | "VERYSILENT" => args.silent = true,
        "SILENT" => args.non_interactive = true,
        _ => {
            // /DIR=<path>（Inno 风格，带引号成单 token）；/D= 由原始命令行尾部处理
            if upper.starts_with("DIR=") {
                let value = rest["DIR=".len()..].trim_matches('"');
                if !value.is_empty() {
                    *slash_target = Some(PathBuf::from(value));
                }
            }
            // 其余未知 slash 开关：丢弃
        }
    }
}

/// NSIS 语义的 `/D=`：必须取原始命令行（值不加引号、可含空格、直到行尾），
/// argv 拆分会把它切碎，无法还原。
fn raw_command_line_d_tail() -> Option<PathBuf> {
    let raw = unsafe { windows::Win32::System::Environment::GetCommandLineW() };
    if raw.is_null() {
        return None;
    }
    let cmdline = unsafe { raw.to_string() }.ok()?;
    d_tail_from(&cmdline)
}

fn d_tail_from(cmdline: &str) -> Option<PathBuf> {
    let lower = cmdline.to_ascii_lowercase();
    let mut search = 0;
    while let Some(pos) = lower[search..].find("/d=") {
        let abs = search + pos;
        let at_token_start = cmdline[..abs]
            .chars()
            .last()
            .is_some_and(|c| c == ' ' || c == '\t');
        if at_token_start {
            let value = cmdline[abs + 3..].trim();
            let value = value
                .strip_prefix('"')
                .and_then(|v| v.strip_suffix('"'))
                .unwrap_or(value);
            if value.is_empty() {
                return None;
            }
            return Some(PathBuf::from(value));
        }
        search = abs + 3;
    }
    None
}

fn help_text() -> String {
    let mut out = String::from("Kachina Installer\n\nUsage: installer.exe [OPTIONS]\n\nOptions:\n");
    for spec in OPTS.iter().filter(|o| !o.hidden) {
        let mut lhs = String::from("  ");
        if let Some(c) = spec.short {
            lhs.push('-');
            lhs.push(c);
        }
        if let Some(l) = spec.long {
            if spec.short.is_some() {
                lhs.push_str(", ");
            }
            lhs.push_str("--");
            lhs.push_str(l);
        }
        if spec.takes_value {
            lhs.push_str(" <");
            lhs.push_str(spec.value_name);
            lhs.push('>');
        }
        out.push_str(&format!("{lhs:<18}{}\n", spec.help));
    }
    out.push_str(&format!("{:<18}Print help\n", "  -h, --help"));
    out
}

fn help_and_exit() -> ! {
    let text = help_text();
    let has_console =
        !unsafe { windows::Win32::System::Console::GetConsoleWindow() }.is_invalid();
    if has_console {
        println!("{text}");
    } else {
        rfd::MessageDialog::new()
            .set_title("Kachina Installer")
            .set_description(&text)
            .show();
    }
    std::process::exit(0);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn os(args: &[&str]) -> Vec<OsString> {
        args.iter().map(OsString::from).collect()
    }

    fn install(argv: &[&str]) -> InstallArgs {
        match parse_from(&os(argv), None) {
            Command::Install(a) | Command::NativeUi(a) => a,
            other => panic!("expected install args, got {other:?}"),
        }
    }

    // ---- 对拍 oracle：镜像旧 clap 定义，同一输入两边解析结果必须一致 ----

    mod oracle {
        use clap::Parser;
        use std::path::PathBuf;

        #[derive(Debug, Clone, clap::Args)]
        pub struct ClapInstall {
            #[clap(short = 'D')]
            pub target: Option<PathBuf>,
            #[clap(short = 'I')]
            pub non_interactive: bool,
            #[clap(short = 'S')]
            pub silent: bool,
            #[clap(short = 'O')]
            pub online: bool,
            #[clap(short = 'U')]
            pub uninstall: bool,
            #[clap(long, hide = true)]
            pub source: Option<String>,
            #[clap(long, hide = true)]
            pub dfs_extras: Option<String>,
            #[clap(long, hide = true)]
            pub mirrorc_cdk: Option<String>,
            #[clap(long, hide = true)]
            pub dump_dir: Option<PathBuf>,
        }

        #[derive(clap::Subcommand, Clone, Debug)]
        pub enum ClapCommand {
            #[clap(hide = true)]
            Install(ClapInstall),
            #[clap(hide = true)]
            InstallWebview2,
            #[clap(hide = true)]
            NativeUi(ClapInstall),
        }

        #[derive(Parser)]
        #[command(args_conflicts_with_subcommands = true)]
        pub struct ClapCli {
            #[command(subcommand)]
            pub command: Option<ClapCommand>,
            #[clap(flatten)]
            pub install: ClapInstall,
        }

        pub fn parse(argv: &[&str]) -> ClapInstall {
            let full: Vec<&str> = std::iter::once("installer.exe")
                .chain(argv.iter().copied())
                .collect();
            let cli = ClapCli::try_parse_from(full).expect("clap oracle parse failed");
            match cli.command {
                Some(ClapCommand::Install(a)) | Some(ClapCommand::NativeUi(a)) => a,
                Some(ClapCommand::InstallWebview2) => panic!("unexpected subcommand"),
                None => cli.install,
            }
        }
    }

    fn assert_matches_clap(argv: &[&str]) {
        let ours = install(argv);
        let theirs = oracle::parse(argv);
        assert_eq!(ours.target, theirs.target, "target mismatch on {argv:?}");
        assert_eq!(
            ours.non_interactive, theirs.non_interactive,
            "non_interactive mismatch on {argv:?}"
        );
        assert_eq!(ours.silent, theirs.silent, "silent mismatch on {argv:?}");
        assert_eq!(ours.online, theirs.online, "online mismatch on {argv:?}");
        assert_eq!(
            ours.uninstall, theirs.uninstall,
            "uninstall mismatch on {argv:?}"
        );
        assert_eq!(ours.source, theirs.source, "source mismatch on {argv:?}");
        assert_eq!(
            ours.dfs_extras, theirs.dfs_extras,
            "dfs_extras mismatch on {argv:?}"
        );
        assert_eq!(
            ours.mirrorc_cdk, theirs.mirrorc_cdk,
            "mirrorc_cdk mismatch on {argv:?}"
        );
        assert_eq!(
            ours.dump_dir, theirs.dump_dir,
            "dump_dir mismatch on {argv:?}"
        );
    }

    #[test]
    fn oracle_parity_on_supported_grammar() {
        let cases: &[&[&str]] = &[
            &[],
            &["-S"],
            &["-I"],
            &["-O"],
            &["-U"],
            &["-SI"],
            &["-IS"],
            &["-SOU"],
            &["-D", r"C:\App"],
            &[r"-DC:\App"],
            &[r"-D=C:\App"],
            &["-S", "-D", r"C:\Program Files\App"],
            &["--source", "cdn"],
            &["--source=cdn"],
            &["--dfs-extras", r#"{"a":1}"#],
            &["--mirrorc-cdk", "KEY123"],
            &["--dump-dir", r"C:\dump"],
            &["-S", "--source=cdn", "-D", r"C:\x"],
            &["install", "-S"],
            &["install", "-D", r"C:\App", "-I"],
            &["native-ui", "-S", "--source", "cdn"],
        ];
        for case in cases {
            assert_matches_clap(case);
        }
    }

    #[test]
    fn subcommands_dispatch() {
        assert!(matches!(
            parse_from(&os(&["install-webview2"]), None),
            Command::InstallWebview2
        ));
        assert!(matches!(
            parse_from(&os(&["native-ui", "-S"]), None),
            Command::NativeUi(a) if a.silent
        ));
        match parse_from(&os(&["headless-uac", "pipe-123"]), None) {
            Command::HeadlessUac(u) => assert_eq!(u.pipe_id, "pipe-123"),
            other => panic!("expected HeadlessUac, got {other:?}"),
        }
    }

    // ---- slash 开关（clap 无此能力，期望值测试） ----

    #[test]
    fn slash_aliases() {
        assert!(install(&["/S"]).silent);
        assert!(install(&["/s"]).silent);
        assert!(install(&["/VERYSILENT"]).silent);
        assert!(install(&["/verysilent"]).silent);
        assert!(install(&["/SILENT"]).non_interactive);
        assert_eq!(
            install(&[r#"/DIR="C:\App""#]).target.as_deref(),
            Some(std::path::Path::new(r"C:\App"))
        );
        assert_eq!(
            install(&[r"/DIR=C:\App"]).target.as_deref(),
            Some(std::path::Path::new(r"C:\App"))
        );
    }

    #[test]
    fn d_tail_parses_unquoted_spaces() {
        assert_eq!(
            d_tail_from(r#""C:\dl\inst.exe" /S /D=C:\Program Files\App"#),
            Some(PathBuf::from(r"C:\Program Files\App"))
        );
        assert_eq!(
            d_tail_from(r#""C:\dl\inst.exe" /d="C:\App""#),
            Some(PathBuf::from(r"C:\App"))
        );
        assert_eq!(d_tail_from(r#""C:\dl\inst.exe" -S"#), None);
        // 前一字符不是空白（路径内出现）不触发
        assert_eq!(d_tail_from(r#""C:\x/d=y\inst.exe""#), None);
    }

    #[test]
    fn target_precedence_dash_wins_over_slash() {
        let argv = os(&["-D", r"C:\Explicit"]);
        match parse_from(&argv, Some(PathBuf::from(r"C:\FromTail"))) {
            Command::Install(a) => {
                assert_eq!(a.target.as_deref(), Some(std::path::Path::new(r"C:\Explicit")))
            }
            other => panic!("{other:?}"),
        }
        let argv = os(&["/S"]);
        match parse_from(&argv, Some(PathBuf::from(r"C:\FromTail"))) {
            Command::Install(a) => {
                assert!(a.silent);
                assert_eq!(a.target.as_deref(), Some(std::path::Path::new(r"C:\FromTail")))
            }
            other => panic!("{other:?}"),
        }
    }

    // ---- 永不失败：未知输入跳过，已知选项保留 ----

    #[test]
    fn lenient_unknown_input() {
        assert_eq!(install(&["--foo"]), InstallArgs::default());
        assert_eq!(install(&["-Z"]), InstallArgs::default());
        assert_eq!(install(&[r"C:\dropped\file.txt"]), InstallArgs::default());
        assert_eq!(install(&["/NCRC"]), InstallArgs::default());
        // 未知 token 不影响已知旗标
        let a = install(&["--typo", "-S"]);
        assert!(a.silent);
        let a = install(&["-ZS"]);
        assert!(a.silent);
    }

    #[test]
    fn help_lists_visible_hides_hidden() {
        let text = help_text();
        assert!(text.contains("-D <DIR>"));
        assert!(text.contains("-S"));
        assert!(text.contains("--help"));
        assert!(!text.contains("--source"));
        assert!(!text.contains("--mirrorc-cdk"));
    }
}
