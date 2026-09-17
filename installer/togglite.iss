; Inno Setup script for Togglite. Compile after `cargo build --release`:
;   iscc installer\togglite.iss        -> dist\Togglite-Setup-<version>.exe
; The version is read from the exe's VERSIONINFO, which build.rs fills from Cargo.toml.

#define ExePath "..\target\release\togglite.exe"
#define AppVersion GetStringFileInfo(ExePath, "ProductVersion")
#if AppVersion == ""
  #error togglite.exe not found or has no version info; run `cargo build --release` first
#endif

[Setup]
AppId={{6F2C0B4E-2D5A-4C1B-9B0E-7A3E5D9C1F42}
AppName=Togglite
AppVersion={#AppVersion}
AppVerName=Togglite {#AppVersion}
AppPublisher=tattsun
AppPublisherURL=https://github.com/tattsun/togglite
AppSupportURL=https://github.com/tattsun/togglite/issues
AppUpdatesURL=https://github.com/tattsun/togglite/releases
VersionInfoVersion={#AppVersion}
VersionInfoDescription=Togglite Setup

; Per-user install under %LOCALAPPDATA%\Programs: no admin prompt.
PrivilegesRequired=lowest
DefaultDirName={autopf}\Togglite
DisableProgramGroupPage=yes
DisableWelcomePage=no
UninstallDisplayName=Togglite
UninstallDisplayIcon={app}\togglite.exe
SetupIconFile=..\icons\idle.ico
MinVersion=10.0
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible

; A running Togglite is asked to exit in [Code] below, so skip the generic
; "applications are using files" page and the Restart Manager relaunch.
CloseApplications=no
RestartApplications=no

OutputDir=..\dist
OutputBaseFilename=Togglite-Setup-{#AppVersion}
Compression=lzma2/max
SolidCompression=yes
WizardStyle=modern
ShowLanguageDialog=auto

[Languages]
Name: "english"; MessagesFile: "compiler:Default.isl"
Name: "japanese"; MessagesFile: "compiler:Languages\Japanese.isl"

[CustomMessages]
english.StartupTask=Start Togglite automatically when you sign in to Windows
japanese.StartupTask=Windows へのサインイン時に Togglite を自動的に起動する
english.RemoveSettings=Do you also want to remove your Togglite settings, including the saved API token?
japanese.RemoveSettings=Togglite の設定（保存された API トークンを含む）も削除しますか？

[Tasks]
Name: "startup"; Description: "{cm:StartupTask}"

[Files]
Source: "{#ExePath}"; DestDir: "{app}"; Flags: ignoreversion
Source: "..\LICENSE"; DestDir: "{app}"; DestName: "LICENSE.txt"

[Icons]
Name: "{autoprograms}\Togglite"; Filename: "{app}\togglite.exe"

[Registry]
Root: HKCU; Subkey: "Software\Microsoft\Windows\CurrentVersion\Run"; ValueType: string; ValueName: "Togglite"; ValueData: """{app}\togglite.exe"""; Flags: uninsdeletevalue; Tasks: startup
Root: HKCU; Subkey: "Software\Microsoft\Windows\CurrentVersion\Run"; ValueType: none; ValueName: "Togglite"; Flags: deletevalue; Tasks: not startup

[Run]
Filename: "{app}\togglite.exe"; Description: "{cm:LaunchProgram,Togglite}"; Flags: nowait postinstall skipifsilent

[Code]
const
  WM_APP = $8000;
  WM_TOGGLITE_QUIT = WM_APP + 2;   // handled by the popup window, see src/ui.rs
  PopupClass = 'TogglitePopup';

function IsRunning(): Boolean;
begin
  Result := FindWindowByClassName(PopupClass) <> 0;
end;

// Asks a running Togglite to exit and waits for it; kills it as a last resort.
procedure CloseRunningInstance();
var
  Wnd: HWND;
  I, ResultCode: Integer;
begin
  Wnd := FindWindowByClassName(PopupClass);
  if Wnd = 0 then
    exit;
  PostMessage(Wnd, WM_TOGGLITE_QUIT, 0, 0);
  for I := 1 to 30 do
  begin
    Sleep(100);
    if not IsRunning() then
      break;
  end;
  if IsRunning() then
    Exec(ExpandConstant('{sys}\taskkill.exe'), '/F /IM togglite.exe', '', SW_HIDE, ewWaitUntilTerminated, ResultCode);
  // Let the process finish unloading before its files are replaced.
  Sleep(500);
end;

function PrepareToInstall(var NeedsRestart: Boolean): String;
begin
  CloseRunningInstance();
  Result := '';
end;

function InitializeUninstall(): Boolean;
begin
  CloseRunningInstance();
  Result := True;
end;

procedure CurUninstallStepChanged(CurUninstallStep: TUninstallStep);
var
  SettingsDir: String;
begin
  if CurUninstallStep = usPostUninstall then
  begin
    SettingsDir := ExpandConstant('{userappdata}\togglite');
    if DirExists(SettingsDir) then
      if SuppressibleMsgBox(CustomMessage('RemoveSettings'), mbConfirmation, MB_YESNO or MB_DEFBUTTON2, IDNO) = IDYES then
        DelTree(SettingsDir, True, True, True);
  end;
end;
