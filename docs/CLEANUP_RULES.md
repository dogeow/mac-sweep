# 清理规则与参考

这份规则把常见清理工具的分类落实为 Mac Sweep 的具体扫描范围。参考官方分类，不声称复刻厂商未公开的识别算法。

- [MacClean：System Junk](https://www.imobie.com/guide/macclean/system-junk.htm) 将系统整理分成用户垃圾、系统垃圾、开发垃圾和应用残留。
- [MacClean：Internet Junk](https://www.imobie.com/guide/macclean/internet-junk.htm) 包含浏览器缓存，也包含历史与 Cookie；Mac Sweep 只采用可重新生成的缓存部分。
- [CleanMyMac：System Junk](https://macpaw.com/support/cleanmymac-x/knowledgebase/system-junk) 说明用户缓存、日志、安装镜像、旧更新、Xcode 和应用支持内容，并对部分内容采用手动选择。
- [MacPaw：CleanMyMac CLI](https://github.com/MacPaw/cleanmymac-cli) 列出 Homebrew、npm、Yarn、pnpm、pip、uv 等工具缓存以及 Xcode 编译内容。项目内部的构建目录属于另一类整理任务。

## 已实现的检查范围

默认保留最近 14 天修改的项目，设置中可调整。整目录必须连同全部子项满足保留天数；新目录中仍可单独发现符合条件的旧文件或完整旧分支。所有候选移动前重新检查身份、父目录和完整递归清单，确认后仅移到废纸篓。

| 规则 | 扫描位置与识别条件 | 选择方式 |
| --- | --- | --- |
| 普通应用旧缓存 | `~/Library/Caches`；跳过模型、数据库、游戏数据与正在运行的应用 | 符合条件的普通缓存为建议项；运行时缓存需确认 |
| 沙盒应用缓存 | `~/Library/Containers/<应用标识>/Data/Library/Caches`，只扫描明确缓存子路径 | 手动确认；不扫描整个容器、Documents 或聊天附件 |
| Electron 等应用缓存 | `~/Library/Application Support/<应用>/Cache`、`Code Cache`、`GPUCache`、`DawnCache`、`DawnGraphiteCache`、`DawnWebGPUCache` | 手动确认；不扫描账户、数据库、IndexedDB、Local Storage 等目录 |
| 浏览器磁盘缓存 | Chrome 的 `~/Library/Caches/Google/Chrome`、`com.google.Chrome`；Firefox 的 `~/Library/Caches/Firefox/Profiles`；Edge 的 `Microsoft Edge`、`com.microsoft.edgemac`；Safari 的 `com.apple.Safari/WebKitCache` | 手动确认；浏览器运行时保留。历史、Cookie、密码、书签和会话文件被排除 |
| Node 工具下载缓存 | `~/.npm/_cacache`、`~/.cache/node-gyp`、`~/.cache/yarn`、`~/.cache/pnpm`、`~/Library/pnpm/store`、`~/Library/Caches/Yarn` | 手动确认；不包含项目 `node_modules` |
| Python 工具缓存 | `~/.cache/pip`、`~/.cache/uv`、`~/Library/Caches/pip`、`uv` | 手动确认；保护模型文件和已识别的模型目录 |
| Rust 与 Go 下载缓存 | `~/.cargo/registry/cache`、`~/go/pkg/mod/cache/download` | 手动确认；不扫描 Cargo 安装程序、注册表源码或整个 Go 工作区 |
| 其他开发缓存 | `~/.gradle/caches`、`~/Library/Caches/Homebrew`、`CocoaPods` | 手动确认，可能需要重新下载或影响离线工作 |
| Xcode 编译缓存 | `~/Library/Developer/Xcode/DerivedData`、`~/Library/Caches/com.apple.dt.Xcode` | 手动确认；不包含 Archives、DeviceSupport 或模拟器数据 |
| 旧日志与崩溃报告 | `~/Library/Logs` 内的 `.log`、`.txt`、`.crash`、`.ips`、`.out`、`.err`、`.trace` 与轮转日志 | 建议项；保留目录与最近日志，不将整日志目录作为候选 |
| 旧安装包 | `~/Downloads` 直属 `.dmg`、`.pkg`、`.mpkg`、`.iso` | 手动确认；普通 ZIP、文档、备份与项目不是安装包 |
| 疑似卸载残留 | Preferences、Application Support、Caches 内的反向域名条目，及 Saved Application State 的 `<标识>.savedState` | 手动确认；应用清单完整、找不到同标识或同开发者应用才列出。缺少应用不等于已经卸载 |

缓存根目录本身保留。符号链接不跟随，多硬链接文件不作为候选；扫描遇到不可读取、结构变化或无法完整重验的分支时保留该分支。新增加的缓存规则一律不默认勾选。

## 覆盖与数量限制

旧版达到 2,000 个候选后会停止所有检查，单个应用的小缓存可以耗尽名额。现在优先合并经过完整验证的旧缓存分支，每个应用来源的每类候选最多 500 个；达到应用限额后继续检查其他应用。整次检查最多 10,000 个候选、1,000,000 次访问或 180 秒，达到限制会说明部分结果。数量限制保证有界检查，不等于本机只有这些可整理内容。

## 保留的内容

不会自动处理系统缓存与日志、`/private/var` 临时目录、应用语言文件或二进制架构、Time Machine/APFS 快照、设备备份、邮件/聊天附件、照片库、Xcode Archives/DeviceSupport、模拟器、Docker 数据、AI 模型和项目内的构建目录。它们需要额外的用途或工具状态判断，不能只按路径名称或文件年龄判成垃圾。可通过目录大小分析查看占用，手动确认具体项目。

卸载残留可能包含个人设置与数据，必须打开查看；本规则没有把它们改为默认建议项。移到废纸篓仍占磁盘空间，清空由用户在 Finder 中自行决定。

应用归属显示优先精确标识，无歧义的辅助应用关系才可提供名称；同厂商只用于保留疑似残留，不把不同应用合成一个默认清理组。正在运行的应用从真实应用包读取标识，不依赖常见安装目录。应用残留扫描仍拒绝 Cookie、历史、登录与数据库文件。候选全局禁止祖先与子项重叠。
