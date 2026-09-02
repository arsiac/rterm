[Setup]
AppId={{d130bf19-d690-44ed-8ffa-f8dee01d9988}
AppName=rterm
AppVersion={#MyAppVersion}
AppPublisher=arsiac
DefaultDirName={autopf}\rterm
DefaultGroupName=rterm
SetupIconFile=..\..\crates\rterm-gui\icons\app\app.ico
UninstallDisplayIcon={app}\rterm.exe
OutputDir=..\..
OutputBaseFilename=rterm-{#MyAppVersion}-windows-amd64
Compression=lzma2
SolidCompression=yes
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible

[Languages]
Name: "english"; MessagesFile: "compiler:Default.isl"
Name: "chinesesimplified"; MessagesFile: "compiler:Languages\ChineseSimplified.isl"

[Tasks]
Name: "desktopicon"; Description: "{cm:CreateDesktopIcon}"; GroupDescription: "{cm:AdditionalIcons}"; Flags: checkedonce

[Files]
Source: "{#BinPath}"; DestDir: "{app}"; Flags: ignoreversion

[Icons]
Name: "{group}\rterm"; Filename: "{app}\rterm.exe"
Name: "{autodesktop}\rterm"; Filename: "{app}\rterm.exe"; Tasks: desktopicon

[Run]
Filename: "{app}\rterm.exe"; Description: "{cm:LaunchAfterInstall}"; Flags: nowait postinstall skipifsilent

[CustomMessages]
english.CreateDesktopIcon=Create a &desktop shortcut
english.AdditionalIcons=Additional icons:
english.LaunchAfterInstall=&Launch rterm after installation
chinesesimplified.CreateDesktopIcon=创建桌面快捷方式(&d)
chinesesimplified.AdditionalIcons=附加图标:
chinesesimplified.LaunchAfterInstall=安装后启动 rterm(&L)
