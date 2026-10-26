; Inno Setup Script for Mudcrab Windows Installer
; Builds a modern Next-Next-Finish setup wizard (.exe)
;
; "Hey you, you're finally awake."

#ifndef AppVersion
  #define AppVersion "0.1.0"
#endif

#ifndef BinDir
  #define BinDir "..\..\target\x86_64-pc-windows-msvc\release"
#endif

#ifndef OutputDir
  #define OutputDir "..\..\"
#endif

[Setup]
; Unique GUID identifying Mudcrab across versions and upgrades
AppId={{8B78124C-FE3D-4A46-95D9-38B132C19C74}}
AppName=Mudcrab
AppVersion={#AppVersion}
AppPublisher=Mudcrab Team
AppPublisherURL=https://mudcrab.dev
AppSupportURL=https://github.com/Mudcrab-Team/mudcrab/issues
AppUpdatesURL=https://mudcrab.dev

; Dual-mode: per-user install by default (no UAC elevation prompt needed),
; or administrative all-users install if run as admin or selected in dialog.
DefaultDirName={autopf}\Mudcrab
DefaultGroupName=Mudcrab
AllowNoIcons=yes
PrivilegesRequiredOverridesAllowed=dialog
PrivilegesRequired=lowest

; License agreement step
LicenseFile=..\..\LICENSE

; Output settings
OutputDir={#OutputDir}
OutputBaseFilename=mudcrab-v{#AppVersion}-windows-x64-setup
Compression=lzma2/ultra64
SolidCompression=yes

; 64-bit architecture enforcement
ArchitecturesInstallIn64BitMode=x64compatible
ArchitecturesAllowed=x64compatible

; UI theme
WizardStyle=modern

[Languages]
Name: "english"; MessagesFile: "compiler:Default.isl"

[Tasks]
Name: "desktopicon"; Description: "{cm:CreateDesktopIcon}"; GroupDescription: "{cm:AdditionalIcons}"; Flags: unchecked

[Files]
Source: "{#BinDir}\launcher.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#BinDir}\engine.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#BinDir}\converter.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#BinDir}\world-inspect.exe"; DestDir: "{app}"; Flags: ignoreversion

[Icons]
Name: "{group}\Mudcrab Launcher"; Filename: "{app}\launcher.exe"; WorkingDir: "{app}"
Name: "{group}\Mudcrab Engine"; Filename: "{app}\engine.exe"; WorkingDir: "{app}"
Name: "{group}\Mudcrab Converter"; Filename: "{app}\converter.exe"; WorkingDir: "{app}"
Name: "{group}\{cm:UninstallProgram,Mudcrab}"; Filename: "{uninstallexe}"
Name: "{autodesktop}\Mudcrab"; Filename: "{app}\launcher.exe"; WorkingDir: "{app}"; Tasks: desktopicon

[Run]
Filename: "{app}\launcher.exe"; Description: "{cm:LaunchProgram,Mudcrab}"; Flags: nowait postinstall skipifsilent
