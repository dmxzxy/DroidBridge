; DroidBridge 的 Windows 安装包（Inno Setup 6）
; 构建：ISCC.exe /DVersion=0.7.0 tools\installer.iss
; 默认安装到当前用户目录（不需要管理员权限），这样程序可以自己下载并替换文件完成更新。

; 版本号由构建脚本通过环境变量 DB_VERSION 传入
#ifndef Version
  #define Version GetEnv("DB_VERSION")
#endif
#if Version == ""
  #define Version "0.0.0"
#endif
#define AppName "DroidBridge"
#define AppExe "DroidBridge.exe"
#define SourceDir "..\dist\DroidBridge"

[Setup]
AppId={{8E1F4C2A-2B7D-4B57-9E36-6B1D2F5C7A11}
AppName={#AppName}
AppVersion={#Version}
AppPublisher=DroidBridge
AppPublisherURL=https://github.com/dmxzxy/DroidBridge
AppSupportURL=https://github.com/dmxzxy/DroidBridge
DefaultDirName={autopf}\{#AppName}
DefaultGroupName={#AppName}
DisableProgramGroupPage=yes
; 按用户安装：程序目录可写，才能自我更新
PrivilegesRequired=lowest
PrivilegesRequiredOverridesAllowed=dialog
OutputDir=..\dist
OutputBaseFilename=DroidBridge-{#Version}-windows-x64-setup
SetupIconFile=..\target\icons\icon.ico
UninstallDisplayIcon={app}\{#AppExe}
Compression=lzma2/max
SolidCompression=yes
WizardStyle=modern
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
; 关掉“程序正在运行”的强制检查：升级时由程序自己在更新前退出
CloseApplications=no
RestartApplications=no

[Tasks]
Name: "desktopicon"; Description: "{cm:CreateDesktopIcon}"; GroupDescription: "{cm:AdditionalIcons}"; Flags: checkedonce
Name: "autostart"; Description: "开机时启动 DroidBridge"; GroupDescription: "启动"; Flags: unchecked

[Files]
Source: "{#SourceDir}\{#AppExe}"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#SourceDir}\droidbridge-server.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#SourceDir}\README.md"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#SourceDir}\LICENSE"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#SourceDir}\scrcpy-LICENSE.txt"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#SourceDir}\start.bat"; DestDir: "{app}"; Flags: ignoreversion skipifsourcedoesntexist
Source: "{#SourceDir}\start-server.bat"; DestDir: "{app}"; Flags: ignoreversion skipifsourcedoesntexist
Source: "{#SourceDir}\platform-tools\*"; DestDir: "{app}\platform-tools"; Flags: ignoreversion recursesubdirs createallsubdirs
; data 目录由程序运行时创建（设备列表、相册、更新缓存），不随安装包分发，卸载时也不删除

[Icons]
Name: "{group}\{#AppName}"; Filename: "{app}\{#AppExe}"
Name: "{group}\卸载 {#AppName}"; Filename: "{uninstallexe}"
Name: "{autodesktop}\{#AppName}"; Filename: "{app}\{#AppExe}"; Tasks: desktopicon

[Registry]
Root: HKCU; Subkey: "Software\Microsoft\Windows\CurrentVersion\Run"; ValueType: string; ValueName: "DroidBridge"; ValueData: """{app}\{#AppExe}"""; Flags: uninsdeletevalue; Tasks: autostart

[Run]
Filename: "{app}\{#AppExe}"; Description: "{cm:LaunchProgram,{#AppName}}"; Flags: nowait postinstall skipifsilent

[UninstallRun]
Filename: "{sys}\taskkill.exe"; Parameters: "/F /IM {#AppExe}"; Flags: runhidden skipifdoesntexist
