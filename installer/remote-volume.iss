; The Windows setup, built by CI with Inno Setup:
;   iscc /DAppVersion=1.2.3 /DSourceExe=<remote-volume.exe> installer\remote-volume.iss
;
; It only delivers the program: `remote-volume --install` does the installing, as on the other OSes
; (a per-user folder, the start at login, stopping a previous version). Per user and without admin
; rights, so that the app can later update its own program file.

#ifndef AppVersion
  #error Pass /DAppVersion=<version>
#endif
#ifndef SourceExe
  #error Pass /DSourceExe=<path to remote-volume.exe>
#endif

[Setup]
AppId={{7C2E5F1A-3B84-4D6E-9A21-5E0F8C47B9D3}
AppName=Remote Volume
AppVersion={#AppVersion}
AppPublisher=JusteReseau
AppPublisherURL=https://github.com/justereseau/companion-remote-volume
DefaultDirName={localappdata}\Programs\RemoteVolume
DisableDirPage=yes
DisableProgramGroupPage=yes
PrivilegesRequired=lowest
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
OutputDir=.
OutputBaseFilename=remote-volume-setup-windows-x64
Compression=lzma2
SolidCompression=yes
WizardStyle=modern
UninstallDisplayName=Remote Volume
UninstallDisplayIcon={app}\remote-volume.exe
; The app replaces its own program file when it updates, so the setup must not manage that file.
CloseApplications=no

[Files]
Source: "{#SourceExe}"; DestDir: "{tmp}"; DestName: "remote-volume.exe"; Flags: ignoreversion deleteafterinstall

[Run]
Filename: "http://127.0.0.1:2502/"; Description: "Open the settings page"; Flags: postinstall shellexec nowait skipifsilent

[UninstallRun]
Filename: "{app}\remote-volume.exe"; Parameters: "--uninstall"; RunOnceId: "RemoveRemoteVolume"; Flags: runhidden waituntilterminated

[UninstallDelete]
Type: filesandordirs; Name: "{app}"

[Code]
procedure CurStepChanged(CurStep: TSetupStep);
var
  ResultCode: Integer;
begin
  if CurStep = ssPostInstall then
  begin
    if not Exec(ExpandConstant('{tmp}\remote-volume.exe'), '--install', '', SW_HIDE, ewWaitUntilTerminated, ResultCode) or (ResultCode <> 0) then
    begin
      SuppressibleMsgBox('Remote Volume could not be set up (error ' + IntToStr(ResultCode) + ').', mbError, MB_OK, IDOK);
    end;
  end;
end;
