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
    // 离线包下载底子，需要固定
    "source": "packed+https://example.com/Kachina.Install.exe",
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
    // 卸载时需要额外删除的其他目录或文件
    "extraUninstallPath": ["${INSTALL_PATH}/log"],// UAC 策略
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
      "Microsoft.VCRedist.2015+.x86"
    ]
}
```
2. 构建更新器，用于打包在便携版内等。更新器不需要被打包到离线包内。
```bat
kachina-builder.exe pack -c kachina.config.json -o Kachina.update.exe
```
3. 构建Metadata、压缩应用文件
```bat
kachina-builder.exe gen -j 8 -i {AppDir} -m metadata.json -o hashed -r {AppId} -t {Version} -u Kachina.update.exe
```
4. 构建离线包
```bat
kachina-builder.exe pack -c kachina.config.json -m metadata.json -d hashed -o Kachina.Install.exe
```
5. 部署离线包到服务器上，确保可以通过json里的url下载到。在目前版本里，你不需要部署压缩产生的`hashed`文件夹和metadata文件，这些文件是在构建过程中临时使用的。
6. 此时第二步得到的更新器可以直接作为在线安装包使用。

## 部分技术细节
安装器的离线包是一个可寻址的文件，其中包含了安装器主体、索引、配置、元数据、程序文件、Patch文件。当安装程序运行时，如果程序没有有内嵌资源，会对配置URL中的离线包进行远程寻址，通过文件头中的索引获取资源，并通过HTTP 206 部分下载需要的内容。如果程序有内嵌资源，程序会对比线上和本地的版本，优先使用本地的资源，并在可行的情况下使用先释放本地资源、随后使用服务器上的更新Patch的形式以减少流量损耗。

安装程序和dfs服务器不是强绑定关系，任何可以通过HTTP提供离线包下载的服务器都可以作为更新服务器。dfs在本项目中仅作为一个获取下载地址的API使用。
