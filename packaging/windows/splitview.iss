; SplitView Windows installer (Inno Setup 6.3+).
; Build with packaging\windows\build-installer.ps1 (it builds, signs and packages),
; or by hand:  ISCC /DAppVersion=0.3.0 packaging\windows\splitview.iss
; Needs target\release\splitview.exe. Output: dist\SplitView_<version>_x64-setup.exe

#ifndef AppVersion
  #error Pass the version: ISCC /DAppVersion=x.y.z splitview.iss
#endif
#define Root "..\.."

[Setup]
AppId={{5B1C2E8A-7F3D-4C59-9A2E-3D6F1B8C4E71}
AppName=SplitView
AppVersion={#AppVersion}
AppVerName=SplitView {#AppVersion}
AppPublisher=SplitView
VersionInfoVersion={#AppVersion}
VersionInfoDescription=SplitView installer
; Per-user, no admin rights — the same place the 0.2.x (Tauri) builds used, so
; an upgrade lands where the old version was.
PrivilegesRequired=lowest
DefaultDirName={localappdata}\SplitView
DisableDirPage=auto
DefaultGroupName=SplitView
DisableProgramGroupPage=yes
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
MinVersion=10.0
SetupIconFile={#Root}\packaging\icons\splitview.ico
UninstallDisplayIcon={app}\splitview.exe
UninstallDisplayName=SplitView
WizardStyle=modern
Compression=lzma2/max
SolidCompression=yes
CloseApplications=yes
RestartApplications=no
OutputDir={#Root}\dist
OutputBaseFilename=SplitView_{#AppVersion}_x64-setup

[Languages]
Name: "english"; MessagesFile: "compiler:Default.isl"

[Tasks]
Name: "desktopicon"; Description: "{cm:CreateDesktopIcon}"; GroupDescription: "{cm:AdditionalIcons}"; Flags: unchecked

[Files]
Source: "{#Root}\target\release\splitview.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#Root}\README.md"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#Root}\config.example.toml"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#Root}\crates\app\assets\fonts\OFL.txt"; DestDir: "{app}\licenses"; DestName: "JetBrainsMono-OFL.txt"; Flags: ignoreversion

[Icons]
Name: "{autoprograms}\SplitView"; Filename: "{app}\splitview.exe"
Name: "{autodesktop}\SplitView"; Filename: "{app}\splitview.exe"; Tasks: desktopicon

[Run]
Filename: "{app}\splitview.exe"; Description: "{cm:LaunchProgram,SplitView}"; Flags: nowait postinstall skipifsilent

; Settings (%APPDATA%\splitview) are kept on uninstall.

[Code]
const
  LegacyKey = 'Software\Microsoft\Windows\CurrentVersion\Uninstall\SplitView';

{ The 0.2.x builds were Tauri/NSIS installers registered under their own key.
  Remove that registration (and its files) first, so Apps & Features doesn't
  list SplitView twice. `_?=` makes the NSIS uninstaller run in place and be
  waited for. }
function PrepareToInstall(var NeedsRestart: Boolean): String;
var
  Uninstaller, Dir: String;
  Code: Integer;
begin
  Result := '';
  { /SKIPLEGACY=1 leaves an existing 0.2.x install alone (test installs). }
  if ExpandConstant('{param:skiplegacy|0}') = '1' then
    exit;
  if RegQueryStringValue(HKCU, LegacyKey, 'UninstallString', Uninstaller) then
  begin
    Uninstaller := RemoveQuotes(Uninstaller);
    if FileExists(Uninstaller) then
    begin
      Dir := ExtractFileDir(Uninstaller);
      Exec(Uninstaller, '/S _?=' + Dir, '', SW_HIDE, ewWaitUntilTerminated, Code);
      DeleteFile(Uninstaller);
    end;
    RegDeleteKeyIncludingSubkeys(HKCU, LegacyKey);
  end;
end;
