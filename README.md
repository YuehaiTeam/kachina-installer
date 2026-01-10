# Kachina Installer

快速、多功能的通用安装程序。

- 离线安装
  - 多线程安装，速度快
  - 安装后校验，避免错误
- 在线安装
  - 分块下载，边下载边解压
- 在线更新
  - 自动比对文件差异，增量更新
  - 支持`HDiffPatch`的文件级差分更新
  - 占用检测、结束进程
  - 只需要提供一个离线安装包链接即可完成以上所有操作，无需额外部署
- 运行库安装
  - 支持自动安装 .Net Runtime/Desktop 和 VCRedist
  - 自动安装Webview2以保证`Tauri`的正常运行
- 混合安装
  - 通过旧版安装包和在线更新直接安装最新版
- 卸载
  - 支持只删除包体内的文件，默认不删除用户数据

#### 使用方式

1. 编写`kachina.config.json`，作为安装器的配置文件

```jsonc
{
  // 离线包下载地址，需要固定
  "source": "https://example.com/Kachina.Install.exe",
  // 注册表中的应用名称
  "appName": "Kachina Installer",
  // 注册表中的发布者
  "publisher": "YuehaiTeam",
  // 注册表中的应用ID
  "regName": "Kachina",
  // 主程序文件名
  "exeName": "Kachina.exe",
  // 卸载程序文件名
  "uninstallName": "Kachina.uninst.exe",
  // 更新器文件名
  "updaterName": "Kachina.update.exe",
  // 默认安装路径，和Program Files相对
  "programFilesPath": "KachinaInstaller",
  // GUI里的标题
  "title": "Kachina Installer",
  // GUI里的副标题
  "description": "快速多功能的安装器",
  // 窗口标题
  "windowTitle": "Kachina Installer 安装程序",
  // 卸载时需要删除的用户数据目录或文件
  "userDataPath": ["${INSTALL_PATH}/User"],
  // 更新时如果文件夹已存在且非空则跳过的目录
  "ignoreFolderPath": ["${INSTALL_PATH}/cache"],
  // 卸载时需要额外删除的其他目录或文件
  "extraUninstallPath": ["${INSTALL_PATH}/log"],
  // UAC 策略
  // prefer-admin: 除非用户安装在%User%、%AppData%、%Documents%、%Desktop%、%Downloads%目录，都请求UAC
  // prefer-user: 只在用户没有权限写入的目录请求UAC
  // force: 强制请求UAC
  "uacStrategy": "prefer-admin",
  // 需要安装的运行库，以下为目前支持的列表
  "runtimes": [
    // .NET 的版本号支持 8/8.0/8.0.13 的格式
    "Microsoft.DotNet.DesktopRuntime.8",
    "Microsoft.DotNet.Runtime.8",
    // VCRedist 只支持以下两种格式
    "Microsoft.VCRedist.2015+.x64",
    "Microsoft.VCRedist.2015+.x86",
  ],
}
```

2. 构建更新器，用于打包在便携版内等。更新器不需要被打包到离线包内。

```bat
kachina-builder.exe pack -c kachina.config.json -o Kachina.update.exe
```

可选：为输出的 exe 设置图标和自定义css/左侧图片：

```bat
kachina-builder.exe pack -c kachina.config.json -o Kachina.update.exe --icon icon.ico -m [custom.css | custom.webp]
```

3. 构建Metadata、压缩应用文件

```bat
kachina-builder.exe gen -j 8 -i {AppDir} -m metadata.json -o hashed -r {AppId} -t {Version} -u Kachina.update.exe
```

4. 构建离线包

```bat
kachina-builder.exe pack -c kachina.config.json -m metadata.json -d hashed -o Kachina.Install.exe
```

** 如果在线包使用了自定义UI/图标，请确保在第二步生成更新器时也使用了相同的UI/图标参数，否则会影响安装器自更新能力 **

1. 部署离线包到服务器上，确保可以通过json里的url下载到。在目前版本里，你不需要部署压缩产生的`hashed`文件夹和metadata文件，这些文件是在构建过程中临时使用的。
2. 此时第二步得到的更新器可以直接作为在线安装包使用。

#### 查看/提取离线包内容（kachina-builder extract）

用于调试/排查打包结果：

- 列出离线包内嵌资源（会同时展示 hash 名称与 metadata 中的原始文件名）：

```bat
kachina-builder.exe extract -i Kachina.Install.exe --list
```

- 解包全部文件到目录（会尽量使用 metadata 的原始文件路径；若无 metadata 则输出 hash 名）：

```bat
kachina-builder.exe extract -i Kachina.Install.exe --all out_dir
```

- 按 metadata 文件名提取指定文件（需要离线包内含 metadata）：

```bat
kachina-builder.exe extract -i Kachina.Install.exe --meta-name "Main.exe"
```

提示：`--name` / `--meta-name` / `--all` / `--list` 四种模式互斥，一次只能用一种。

#### 多安装源

如果你希望用户可以自由选择安装源，你可以指定多个Source，此时用户主动打开安装器时将在路径选择上方看到安装源选择按钮。

示例配置如下：

```
{
  "source": [
    {
      "id": "stable",
      "name": "正式版",
      "uri": "https://example.com/Kachina.Install.exe"
    },
    {
      "id": "beta",
      "name": "测试版",
      "uri": "https://example.com/Kachina.Install.Beta.exe"
    }
  ]
}
```

#### Mirror酱平台支持

[Mirror酱](https://mirrorchyan.com) 是独立的第三方软件下载平台，提供付费的软件下载加速服务。`kachina-installer`接入了Mirror酱的API，允许用户使用Mirror酱更新软件。例如，你可以结合上述的安装源选择功能，让用户选择使用自建服务器更新还是使用Mirror酱更新。

如需使用，请设置`source`的值为`mirrorc://{rid}?channel={stable|beta|alpha}`。同时，你需要将前述产生的`.metadata.json`放置到上传给Mirror酱的文件中。示例的上传格式：

```
upload_to_mirrorc.zip
 - .metadata.json
 - Main.exe
 - Main.update.exe
```

也支持

```
upload_to_mirrorc.zip
 - App/.metadata.json
 - App/Main.exe
 - App/Main.update.exe
```

Tips：Mirror酱使用独立的文件级增量更新机制，因此当选择Mirror酱作为更新源时候，将无法使用`kachina-installer`自带的版本比对、二进制Patch级增量等功能。

## 部分技术细节

安装器的离线包是一个可寻址的文件，其中包含了安装器主体、索引、配置、元数据、程序文件、Patch文件。当安装程序运行时，如果程序没有有内嵌资源，会对配置URL中的离线包进行远程寻址，通过文件头中的索引获取资源，并通过HTTP 206 部分下载需要的内容。如果程序有内嵌资源，程序会对比线上和本地的版本，优先使用本地的资源，并在可行的情况下使用先释放本地资源、随后使用服务器上的更新Patch的形式以减少流量损耗。

安装程序和dfs服务器不是强绑定关系，任何可以通过HTTP提供离线包下载的服务器都可以作为更新服务器。dfs在本项目中仅作为一个获取下载地址的API使用。

更多技术细节可以看看 [![Ask DeepWiki](https://deepwiki.com/badge.svg)](https://deepwiki.com/YuehaiTeam/kachina-installer) ，我觉得DeepWiki写得挺好的。
