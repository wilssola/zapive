; Zapive Windows installer, compiled by ISCC in the release workflow:
;   ISCC.exe /DAppVersion=<x.y.z> packaging\windows\zapive.iss
; Paths are relative to this file; the setup exe lands in ..\..\dist.

#ifndef AppVersion
  #define AppVersion "0.0.0"
#endif

[Setup]
AppId={{7C4B54E2-98D1-4CF5-9F65-2E5B7A0C11ED}
AppName=Zapive
AppVersion={#AppVersion}
AppPublisher=Wilssola
AppPublisherURL=https://github.com/wilssola/zapive
AppSupportURL=https://github.com/wilssola/zapive/issues
; Per-user install (LocalAppData\Programs): no UAC prompt, and the
; self-updater can swap the exe in place without elevation.
PrivilegesRequired=lowest
DefaultDirName={autopf}\Zapive
DisableDirPage=yes
DefaultGroupName=Zapive
DisableProgramGroupPage=yes
OutputDir=..\..\dist
OutputBaseFilename=Zapive-Setup-{#AppVersion}
SetupIconFile=..\..\ui\zapive.ico
UninstallDisplayIcon={app}\zapive.exe
Compression=lzma2
SolidCompression=yes
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
CloseApplications=yes
WizardStyle=modern

[Languages]
Name: "english"; MessagesFile: "compiler:Default.isl"
Name: "brazilianportuguese"; MessagesFile: "compiler:Languages\BrazilianPortuguese.isl"

[Tasks]
Name: "desktopicon"; Description: "{cm:CreateDesktopIcon}"; GroupDescription: "{cm:AdditionalIcons}"

[Files]
Source: "..\..\target\release\zapive.exe"; DestDir: "{app}"; Flags: ignoreversion

[Icons]
Name: "{autoprograms}\Zapive"; Filename: "{app}\zapive.exe"
Name: "{autodesktop}\Zapive"; Filename: "{app}\zapive.exe"; Tasks: desktopicon

[Run]
Filename: "{app}\zapive.exe"; Description: "{cm:LaunchProgram,Zapive}"; Flags: nowait postinstall skipifsilent
