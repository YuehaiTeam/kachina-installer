# Kachina Installer

快速、多功能的通用安装程序。

 - 离线安装
   - 多线程安装，速度快
 - 在线安装
   - 分块下载，边下载边解压
 - 在线更新
   - 自动比对文件差异，增量更新
   - 支持`HDiffPatch`的文件级差分更新
 - 混合安装
   - 通过旧版安装包和在线更新直接安装最新版
 - 卸载
   - 支持只删除包体内的文件，默认不删除用户数据

在线功能需配合 [dfs分发管理服务](https://github.com/YuehaiTeam/dfs) 和 [natasync文件同步工具](https://github.com/YuehaiTeam/natasync) 使用。