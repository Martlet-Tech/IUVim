# 安装包与发布

> 61 号任务书（`docs/plan/61-release-v0.2.md`）的落地说明。
> 一句话：**`scripts\package.ps1` 出包，打 tag，传 Release。**

## 1. 出包

```powershell
# 前置（一次性）：Inno Setup 6
winget install --id JRSoftware.InnoSetup -e

# 构建 + 出包（普通窗口，不需要管理员）
scripts\package.ps1

# 只改 .iss 后快速重出（跳过构建）
scripts\package.ps1 -SkipBuild
```

产物：`dist\iuv-setup-<版本>.exe`（约 18 MB —— 词库 36 MB 经 LZMA 压缩后约 12 MB）。

版本号**唯一真相源** = 根 `Cargo.toml` 的 `[workspace.package] version`。
改版本只改这一处：8 个 crate 用 `version.workspace = true` 继承，TSF 的 Windows
版本资源也从 `CARGO_PKG_VERSION` 派生（`platforms/windows/iuv-tsf/build.rs`）。

## 2. 安装包里装了什么

| 内容 | 目标 | 说明 |
|---|---|---|
| 内容 | 目标 | 说明 |
|---|---|---|
| `iuv_tsf.dll` | `{app}`（经 `payload\` 热替换） | x64 TSF，`regsvr32` 注册 |
| `iuv_tsf_x86.dll` | `{app}`（同上） | x86（WoW64），`SysWOW64\regsvr32` 注册 |
| `iuv-server.exe` | `{app}`（同上） | 引擎服务进程 |
| `iuv.imedic` / `iuv.opencc` | `%LOCALAPPDATA%\iuv` | 词库与简繁表（用户数据，不覆盖已有） |
| `scripts\*.ps1` | `{app}\scripts` | 装后动作与卸载动作 |

### ⚠️ 二进制为什么先落 `payload\` 再热替换

**输入法的硬约束：TSF DLL 被全系统运行中的进程映射**（explorer、ctfmon、每个加载过
输入法的应用都持着 image section）。Inno 的 `[Files]` 覆盖已存在文件走
`DeleteFile` + 重建，此时必然 sharing violation → 弹「DeleteFile 失败：错误代码 5。
拒绝访问」，**重试永远过不去**（2026-10-21 真机覆盖安装卡死）。

仓库脚本早就解过这题：`iuv-common.ps1` 的 `Replace-InUseFile`。已加载的 DLL 是以
`FILE_SHARE_DELETE` 映射的，**可以改名**——把旧文件改名成 `.old` 让出原名，再在原位
写新文件。老进程继续持旧映射，新进程加载新 DLL，零杀进程、免注销。

所以 `[Files]` 只把二进制放进 `{app}\payload\`，由 `install-steps.ps1` 做改名替换，
用完删掉 `payload`。**不要为了省事把它们直接写回 `[Files]` 的 `{app}`**——那正是
0.2.0 覆盖安装失败的原因。

装后动作（`scripts/install-steps.ps1`，与 `dev-deploy`/`install` 共用 `iuv-common.ps1`）：

1. 把 `payload\` 里的三个二进制热替换到 `{app}`（改名旧文件 → 原位写新文件）
2. 双架构 `regsvr32` 注册（必须在替换之后：regsvr32 是新进程，加载新文件）
3. 注册计划任务 `Iuv-ServerStart`：登录自启 + **1 分钟周期自愈**
4. 通过计划任务启动 `iuv-server`（关键：必须是**中完整性**上下文，否则 TSF 连不上管道）
5. 缺失时生成默认 `config.json`（已有配置不动——覆盖安装不能丢用户设置）
6. 以中完整性重启 `ctfmon`，让输入法列表刷新

## 3. 发布流程

```powershell
# 1) 改版本（唯一一处）
#    Cargo.toml: [workspace.package] version = "0.2.0"
cargo check --workspace           # 让 Cargo.lock 跟上

# 2) 出包
scripts\package.ps1

# 3) 提交 + 打 tag + 推
git add -A; git commit -m "chore(release): v0.2.0"
git tag v0.2.0
git push origin main --tags

# 4) 建 Release 并上传 dist\iuv-setup-0.2.0.exe
gh release create v0.2.0 dist\iuv-setup-0.2.0.exe --title "iuv 输入法 v0.2.0" --notes-file - 
```

`gh` 需要在有上传权限的账户下 `gh auth login` 一次。

## 4. 已知限制（本版接受）

- **无代码签名** → SmartScreen 首次运行会警告。点「更多信息 → 仍要运行」。
- **`{localappdata}` 跟着提权账户走**：UAC 提权到**另一个**管理员账户时，词库会装到
  那个账户的 `%LOCALAPPDATA%\iuv`，登录用户打字时找不到词库（日志报"词库加载失败"）。
  用同一账户提权即正常。彻底解决要改成 per-user 安装或把词库挪到 `%ProgramData%`，
  留待 v0.3。
- **无静默升级**：本版无自动更新，重新下载新版安装包覆盖安装即可（升级会保留词库与用户数据）。
- **卸载需注销一次收尾**：输入法 DLL 被所有运行中的程序映射，无法当场删除。

## 5. 故障排查

| 症状 | 排查 |
|---|---|
| 装完列表里没有「IUV 输入法」 | 注销/重启一次；或看 `%TEMP%\iuv-script.log` |
| 能切到输入法但只能打英文 | `iuv-server` 没起来：`tasklist \| findstr iuv-server`；看 `%TEMP%\iuv-server-<开机>.log` |
| 打字卡住/候选窗不出现 | 同上日志；`%TEMP%\iuv-tsf-<开机>.log` 看 `[backend]` 行 |
| 词库加载失败 | 检查 `%LOCALAPPDATA%\iuv\iuv.imedic` 是否存在；见上面第 4 节的账户问题 |
| 卸载后文件还在 | 正常：注销/重启时自动清理（`docs/plan/02-conventions.md` §3 的延迟清理机制） |
| 安装中途报「DeleteFile 失败：错误代码 5」 | **0.2.0 的已知缺陷**，0.2.1 已修。升级到 0.2.1 即可覆盖安装；旧版只能先卸载再装 |
| 安装目录出现 `iuv_tsf.dll.old` | 正常：改名替换的产物，注销/重启后由延迟清理删除 |
| 装完提示「无法替换 xxx（被占用且不允许改名）」 | 有进程以独占方式持有该 DLL（少见）。注销后重装 |

日志文件名里的 `<开机>` 是本次开机的 Unix 秒标识，由 `iuv-server` 发布。
