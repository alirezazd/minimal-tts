; Windows installer — compiled by scripts/build-windows.ps1, which passes:
;   /DAppVersion=0.1.4  /DSourceDir=<assembled app folder>
;   /DIconFile=<.ico build.rs rendered>  /DOutputDir=dist  /DOutputName=<file stem>
;
; Per-user by default, like VS Code's user installer: no UAC prompt, lands in
; %LOCALAPPDATA%\Programs, shows up in Settings > Apps with an uninstaller.
; The dialog still offers an all-users install to those with admin rights.

[Setup]
; Never change AppId: it is how upgrades find the existing install.
AppId={{70EB9F6C-89DB-4B50-80E4-324A04AA632D}
AppName=Minimal TTS
AppVersion={#AppVersion}
AppPublisher=Alireza
AppPublisherURL=https://github.com/alirezazd/minimal-tts
AppSupportURL=https://github.com/alirezazd/minimal-tts/issues
AppUpdatesURL=https://github.com/alirezazd/minimal-tts/releases
DefaultDirName={autopf}\Minimal TTS
DisableProgramGroupPage=yes
PrivilegesRequired=lowest
PrivilegesRequiredOverridesAllowed=dialog
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
MinVersion=10.0
SetupIconFile={#IconFile}
UninstallDisplayIcon={app}\minimal-tts.exe
UninstallDisplayName=Minimal TTS
WizardStyle=modern
; the fp32 model is nearly incompressible; lzma2/max still trims the rest
Compression=lzma2/max
SolidCompression=yes
; an upgrade closes a running copy instead of failing on locked files
CloseApplications=yes
OutputDir={#OutputDir}
OutputBaseFilename={#OutputName}

[Tasks]
Name: desktopicon; Description: "{cm:CreateDesktopIcon}"; GroupDescription: "{cm:AdditionalIcons}"; Flags: unchecked

[Files]
Source: "{#SourceDir}\*"; DestDir: "{app}"; Flags: ignoreversion recursesubdirs createallsubdirs

[Icons]
Name: "{autoprograms}\Minimal TTS"; Filename: "{app}\minimal-tts.exe"
Name: "{autodesktop}\Minimal TTS"; Filename: "{app}\minimal-tts.exe"; Tasks: desktopicon

[Run]
Filename: "{app}\minimal-tts.exe"; Description: "{cm:LaunchProgram,Minimal TTS}"; Flags: nowait postinstall skipifsilent
