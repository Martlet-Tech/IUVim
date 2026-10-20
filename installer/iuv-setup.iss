; IUV 输入法 安装脚本（Inno Setup 6）——61 号 §3.2
;
; 产物：dist\iuv-setup-<version>.exe（单文件，含 x64/x86 TSF + server + 词库 + 简繁表）
;
; 与 scripts\install.ps1 的对应关系（改一处必须同步另一处）：
;  · 注册走 regsvr32（双架构各一次；32 位必须用 SysWOW64 才能落 WoW64 视图）；
;  · 词库不进 Program Files：36MB 词库 + 1.3MB 简繁表属用户数据，装到
;    %LOCALAPPDATA%\iuv（与 iuv-core::paths::iuv_dir 一致）；
;  · server 用**计划任务**自启，不能 [Run] 直接起——提权进程直接启动会造出高完整性
;    管道，中完整性 TSF 连不上（2026-09-27 实测教训）；
;  · 计划任务带崩溃重启（61 号 §3.3）：RestartCount=3 / Interval=1min。
;
; **装后动作全部交给 scripts\install-steps.ps1**，不在本文件的 [Code] 里重写一遍：
; 计划任务的 RestartOnFailure 只能用 XML 表达，而 Inno 的 Pascal 脚本写 UTF-16 文件
; 有坑；更重要的是那样会和 iuv-common.ps1 的 Deploy-IuvServer 形成两份真相。
;
; 不签名：无证书，SmartScreen 会拦一次，README 里说明。

#define AppName "IUV 输入法"
#define AppId "{{C69735F1-BAB1-458B-89FC-099ABA877ECB}"
; 版本由 scripts\package.ps1 以 /DAppVersion=x.y.z 传入（真相源 = Cargo.toml workspace.version）
#ifndef AppVersion
  #define AppVersion "0.0.0"
#endif
#define Publisher "Martlet-Tech"
#define ProjectUrl "https://github.com/Martlet-Tech/vaim"

[Setup]
AppId={#AppId}
AppName={#AppName}
AppVersion={#AppVersion}
AppPublisher={#Publisher}
AppPublisherURL={#ProjectUrl}
AppSupportURL={#ProjectUrl}
DefaultDirName={autopf}\iuv
DefaultGroupName={#AppName}
DisableProgramGroupPage=yes
; 必须管理员：写 HKLM\...\CTF\TIP + 注册计划任务
PrivilegesRequired=admin
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
OutputDir=..\dist
OutputBaseFilename=iuv-setup-{#AppVersion}
SetupIconFile=..\platforms\windows\iuv-tsf\res\icon.ico
UninstallDisplayIcon={app}\iuv_tsf.dll
UninstallDisplayName={#AppName}
Compression=lzma2/max
SolidCompression=yes
WizardStyle=modern
MinVersion=10.0
; {localappdata} 在 admin 安装模式下是「提权进程所属用户」的 profile。UAC 提权保持
; 同一用户（管理员批准模式）时正确；**若用户改用另一个管理员账户提权，词库会落到
; 那个账户的 LocalAppData 下**——这是本版已知限制（61 号 §5）。日志能看出来
;（"词库加载失败"），重装时用同一账户即可。关掉 Inno 的这条提示以免噪声。
UsedUserAreasWarning=no

[Languages]
; Inno 官方安装包不带简体中文（Languages\ 里没有 ChineseSimplified.isl）。
; 用仓库内自带的翻译文件（来源 kira-96/Inno-Setup-Chinese-Simplified-Translation）。
; 文件必须是 UTF-8 + BOM，否则中文乱码。
Name: "chinesesimplified"; MessagesFile: "ChineseSimplified.isl"
Name: "english"; MessagesFile: "compiler:Default.isl"

[Files]
; 三个二进制产物（scripts\package.ps1 从 target / target-server 汇集到 build\pkg）
Source: "..\build\pkg\iuv_tsf.dll";     DestDir: "{app}"; Flags: ignoreversion
Source: "..\build\pkg\iuv_tsf_x86.dll"; DestDir: "{app}"; Flags: ignoreversion
Source: "..\build\pkg\iuv-server.exe";  DestDir: "{app}"; Flags: ignoreversion
; 词库与简繁表 → 用户数据目录。onlyifdoesntexist：升级不覆盖用户已有的词库；
; uninsneveruninstall：不归 Inno 卸载跟踪（卸载时由 uninstall.ps1 备份后清理）。
Source: "..\build\pkg\iuv.imedic"; DestDir: "{localappdata}\iuv"; Flags: ignoreversion onlyifdoesntexist uninsneveruninstall
Source: "..\build\pkg\iuv.opencc"; DestDir: "{localappdata}\iuv"; Flags: ignoreversion onlyifdoesntexist uninsneveruninstall
; 脚本随装：install-steps.ps1（装后动作）+ uninstall.ps1（卸载动作）+ 共用库
Source: "..\scripts\install-steps.ps1"; DestDir: "{app}\scripts"; Flags: ignoreversion
Source: "..\scripts\uninstall.ps1";     DestDir: "{app}\scripts"; Flags: ignoreversion
Source: "..\scripts\iuv-common.ps1";    DestDir: "{app}\scripts"; Flags: ignoreversion

[Run]
; 1) 注册：x64 用 System32 regsvr32，x86 用 SysWOW64（落 WoW64 视图）
Filename: "{sys}\regsvr32.exe";     Parameters: "/s ""{app}\iuv_tsf.dll""";     Flags: runhidden waituntilterminated; StatusMsg: "正在注册输入法（x64）..."
Filename: "{syswow64}\regsvr32.exe"; Parameters: "/s ""{app}\iuv_tsf_x86.dll"""; Flags: runhidden waituntilterminated; StatusMsg: "正在注册输入法（x86）..."
; 2) 计划任务 + 起 server + 默认配置 + 重启 ctfmon（全部在 install-steps.ps1）
Filename: "{sys}\WindowsPowerShell\v1.0\powershell.exe"; \
  Parameters: "-NoProfile -ExecutionPolicy Bypass -File ""{app}\scripts\install-steps.ps1"""; \
  Flags: runhidden waituntilterminated; StatusMsg: "正在配置引擎服务..."

[UninstallRun]
; 卸载统一交给 uninstall.ps1（停服务 / 删 5 个注册表键 / 备份用户数据 / 延迟清理）
Filename: "{sys}\WindowsPowerShell\v1.0\powershell.exe"; \
  Parameters: "-NoProfile -ExecutionPolicy Bypass -File ""{app}\scripts\uninstall.ps1"" -FromUninstaller"; \
  Flags: runhidden waituntilterminated; RunOnceId: "IuvCleanup"

[UninstallDelete]
Type: filesandordirs; Name: "{app}\scripts"

[Code]
// 升级场景：先停旧 server，避免文件占用（Inno 的 CloseApplications 不覆盖常驻进程）。
function PrepareToInstall(var NeedsRestart: Boolean): String;
var
  Code: Integer;
begin
  Exec(ExpandConstant('{sys}\taskkill.exe'), '/f /im iuv-server.exe', '', SW_HIDE,
    ewWaitUntilTerminated, Code);
  Result := '';
end;
