#define AppName "Minnty Sound Server"
#ifndef ArtifactVersion
#define ArtifactVersion "dev"
#endif
#ifndef RepoRoot
#define RepoRoot "..\.."
#endif

[Setup]
AppId={{8E7AF547-C911-40C1-9F5E-7E56B0CE47BF}
AppName={#AppName}
AppVersion={#ArtifactVersion}
AppPublisher=Minnty Sound Server Contributors
DefaultDirName={autopf}\Minnty Sound Server
DefaultGroupName={#AppName}
DisableProgramGroupPage=yes
OutputDir={#RepoRoot}\dist
OutputBaseFilename=minnty-sound-server-{#ArtifactVersion}-windows-x86_64-setup
Compression=lzma2
SolidCompression=yes
WizardStyle=modern
ArchitecturesAllowed=x64
ArchitecturesInstallIn64BitMode=x64
PrivilegesRequired=admin

[Files]
Source: "{#RepoRoot}\target\release\minnty_sound_server.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#RepoRoot}\README.md"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#RepoRoot}\LICENSE"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#RepoRoot}\dist\vc_redist.x64.exe"; DestDir: "{tmp}"; Flags: deleteafterinstall

[Icons]
Name: "{group}\{#AppName}"; Filename: "{app}\minnty_sound_server.exe"
Name: "{commondesktop}\{#AppName}"; Filename: "{app}\minnty_sound_server.exe"; Tasks: desktopicon

[Tasks]
Name: "desktopicon"; Description: "Create a desktop shortcut"; GroupDescription: "Additional shortcuts:"; Flags: unchecked

[Run]
Filename: "{tmp}\vc_redist.x64.exe"; Parameters: "/install /quiet /norestart"; StatusMsg: "Installing Microsoft Visual C++ Redistributable..."; Flags: waituntilterminated
Filename: "{app}\minnty_sound_server.exe"; Description: "Launch {#AppName}"; Flags: nowait postinstall skipifsilent

[Code]
function UpdateReadyMemo(Space, NewLine, MemoUserInfoInfo, MemoDirInfo, MemoTypeInfo, MemoComponentsInfo, MemoGroupInfo, MemoTasksInfo: String): String;
begin
  Result := '';
  if MemoDirInfo <> '' then
    Result := Result + MemoDirInfo + NewLine + NewLine;
  if MemoTasksInfo <> '' then
    Result := Result + MemoTasksInfo + NewLine + NewLine;
  Result := Result + 'Additional components:' + NewLine +
    Space + 'Microsoft Visual C++ Redistributable (x64) will be installed if not already present.';
end;
