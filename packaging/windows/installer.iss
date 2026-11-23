; Windows installer for Gentle Shell (Inno Setup 6). Per-user: no admin rights,
; installs into %LOCALAPPDATA%\Programs\Gentle Shell, so the in-app updater can
; replace the exe there. Built by the Release workflow:
;   ISCC /DAppVersion=<version> /DSourceExe=<path to gentle-shell-gpui.exe>
;        /DOutputDir=<dir> /DIconFile=<path to icon.ico> installer.iss
; Output: gentle-shell-gpui-<version>-windows-x86_64-setup.exe

#ifndef AppVersion
  #error AppVersion is required (/DAppVersion=...)
#endif

[Setup]
; Never change: Windows matches upgrades and the uninstaller by this id.
AppId={{EA8317C7-3FC6-4F38-B4D6-C32357A71260}
AppName=Gentle Shell
AppVersion={#AppVersion}
AppVerName=Gentle Shell {#AppVersion}
AppPublisher=Ismael Osuna
AppPublisherURL=https://github.com/ismaelosuna7824/gentle-shell-gpui
AppSupportURL=https://github.com/ismaelosuna7824/gentle-shell-gpui/issues
AppUpdatesURL=https://github.com/ismaelosuna7824/gentle-shell-gpui/releases
PrivilegesRequired=lowest
DefaultDirName={localappdata}\Programs\Gentle Shell
DisableProgramGroupPage=yes
DisableDirPage=auto
OutputDir={#OutputDir}
OutputBaseFilename=gentle-shell-gpui-{#AppVersion}-windows-x86_64-setup
SetupIconFile={#IconFile}
UninstallDisplayIcon={app}\gentle-shell-gpui.exe
UninstallDisplayName=Gentle Shell
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
WizardStyle=modern
Compression=lzma2/max
SolidCompression=yes
; Close a running Gentle Shell before replacing it, and start it again after.
CloseApplications=yes
RestartApplications=yes
; `gsd` goes on the user PATH: tell running programs the environment changed.
ChangesEnvironment=yes

[Tasks]
Name: "desktopicon"; Description: "{cm:CreateDesktopIcon}"; GroupDescription: "{cm:AdditionalIcons}"

[Files]
Source: "{#SourceExe}"; DestDir: "{app}"; Flags: ignoreversion
; `gsd`: open Gentle Shell from any terminal folder (see crates/app/src/platform/gsd.rs).
Source: "gsd.cmd"; DestDir: "{app}\bin"; Flags: ignoreversion

[Icons]
Name: "{autoprograms}\Gentle Shell"; Filename: "{app}\gentle-shell-gpui.exe"
Name: "{autodesktop}\Gentle Shell"; Filename: "{app}\gentle-shell-gpui.exe"; Tasks: desktopicon

[Run]
Filename: "{app}\gentle-shell-gpui.exe"; Description: "{cm:LaunchProgram,Gentle Shell}"; Flags: nowait postinstall skipifsilent

[UninstallDelete]
; The previous exe an in-app update leaves until the next start.
Type: files; Name: "{app}\gentle-shell-gpui.exe.old"

[Registry]
; Append {app}\bin to the user PATH once; the uninstaller takes it out again.
Root: HKCU; Subkey: "Environment"; ValueType: expandsz; ValueName: "Path"; ValueData: "{olddata};{app}\bin"; Check: NeedsAddPath(ExpandConstant('{app}\bin'))

[Code]
function PathHasEntry(Path, Entry: string): Boolean;
begin
  Result := Pos(';' + Uppercase(Entry) + ';', ';' + Uppercase(Path) + ';') > 0;
end;

function NeedsAddPath(Entry: string): Boolean;
var
  Path: string;
begin
  if not RegQueryStringValue(HKCU, 'Environment', 'Path', Path) then
    Path := '';
  Result := not PathHasEntry(Path, Entry);
end;

procedure CurUninstallStepChanged(CurUninstallStep: TUninstallStep);
var
  Path, Entry: string;
  P: Integer;
begin
  if CurUninstallStep <> usUninstall then
    exit;
  if not RegQueryStringValue(HKCU, 'Environment', 'Path', Path) then
    exit;
  Entry := ExpandConstant('{app}\bin');
  Path := ';' + Path + ';';
  P := Pos(';' + Uppercase(Entry) + ';', Uppercase(Path));
  if P = 0 then
    exit;
  Delete(Path, P, Length(Entry) + 1);
  Path := Copy(Path, 2, Length(Path) - 2);
  RegWriteExpandStringValue(HKCU, 'Environment', 'Path', Path);
end;
