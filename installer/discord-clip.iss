; Inno Setup script for Discord Clip. Built by .github/workflows/build.yml:
;   iscc /DAppVersion=0.3.0 /DSourceExe=..\target\release\discord-clip.exe installer\discord-clip.iss
;
; Per-user install (no admin) into %LOCALAPPDATA%\Programs\discord-clip, the
; same place the old Install.cmd used, so it upgrades those installs in place.
;
; Updating: running a newer setup over an existing install replaces it. For
; in-app updates, run it with /VERYSILENT /SUPPRESSMSGBOXES /NORESTART: it
; closes the running app, installs, and starts the new version.

#ifndef AppVersion
  #define AppVersion "0.0.0-dev"
#endif
#ifndef NumericVersion
  #define NumericVersion "0.0.0"
#endif
#ifndef SourceExe
  #define SourceExe "..\target\release\discord-clip.exe"
#endif
#ifndef OutputName
  #define OutputName "discord-clip-setup"
#endif

[Setup]
; Never change AppId: it's how setup recognises an existing install.
AppId={{38142F7C-3CA2-41C1-8722-C125EADD90CD}
AppName=Discord Clip
AppVersion={#AppVersion}
AppVerName=Discord Clip {#AppVersion}
AppPublisher=Nikibudd
AppPublisherURL=https://github.com/Nikibudd/discord-clip
AppSupportURL=https://github.com/Nikibudd/discord-clip/issues
AppUpdatesURL=https://github.com/Nikibudd/discord-clip/releases
VersionInfoVersion={#NumericVersion}
DefaultDirName={localappdata}\Programs\discord-clip
DisableDirPage=yes
DisableProgramGroupPage=yes
DisableReadyPage=yes
PrivilegesRequired=lowest
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
; Process loopback capture needs Windows 10 2004.
MinVersion=10.0.19041
OutputDir=..\dist
OutputBaseFilename={#OutputName}
UninstallDisplayName=Discord Clip
UninstallDisplayIcon={app}\discord-clip.exe
Compression=lzma2
SolidCompression=yes
WizardStyle=modern
SetupMutex=DiscordClipSetup
; We close the app ourselves (PrepareToInstall) - it lives in the tray and
; doesn't answer Restart Manager shutdown requests.
CloseApplications=no

[Tasks]
Name: autostart; Description: "Start Discord Clip when I sign in to Windows"

[Files]
Source: "{#SourceExe}"; DestDir: "{app}"; DestName: "discord-clip.exe"; Flags: ignoreversion

[Icons]
Name: "{autoprograms}\Discord Clip"; Filename: "{app}\discord-clip.exe"; Comment: "Instant replay for Discord voice chat"

[Registry]
Root: HKCU; Subkey: "Software\Microsoft\Windows\CurrentVersion\Run"; ValueType: string; ValueName: "Discord Clip"; ValueData: """{app}\discord-clip.exe"""; Tasks: autostart; Flags: uninsdeletevalue

[Run]
Filename: "{app}\discord-clip.exe"; Description: "Start Discord Clip now"; Flags: nowait postinstall skipifsilent
; Silent installs are updates started from the app: bring it back up.
Filename: "{app}\discord-clip.exe"; Flags: nowait; Check: WizardSilent

[UninstallRun]
Filename: "{sys}\taskkill.exe"; Parameters: "/F /IM discord-clip.exe"; Flags: runhidden; RunOnceId: "CloseDiscordClip"

[Code]
procedure CloseRunningApp();
var
  Code: Integer;
begin
  Exec(ExpandConstant('{sys}\taskkill.exe'), '/F /IM discord-clip.exe', '', SW_HIDE, ewWaitUntilTerminated, Code);
  Sleep(500);
end;

function PrepareToInstall(var NeedsRestart: Boolean): String;
begin
  CloseRunningApp();
  Result := '';
end;

procedure CurStepChanged(CurStep: TSetupStep);
begin
  // If autostart was unticked, drop a Run entry left by an earlier install.
  if (CurStep = ssPostInstall) and not WizardIsTaskSelected('autostart') then
    RegDeleteValue(HKEY_CURRENT_USER, 'Software\Microsoft\Windows\CurrentVersion\Run', 'Discord Clip');
end;
