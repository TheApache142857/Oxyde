; Inno Setup: the Windows installer, from dist\Oxyde (made by bundle.ps1).
;   iscc /DAppVersion=0.1.0 packaging\installer.iss  ->  dist\Oxyde-Setup-0.1.0.exe
; Installs for the current user only (no administrator rights), into
; %LOCALAPPDATA%\Programs\Oxyde, with a Start menu entry, and opens .oxyprt and .oxyasm
; files (and the older .oxy) with Oxyde.
; Flow studies run OpenFOAM, a separate program (GPL-3.0, OpenCFD Ltd): the task "openfoam"
; downloads OpenCFD's own Windows installer from openfoam.com, checks it, and runs it after
; Oxyde is installed. OpenFOAM is never inside this installer (docs/CFD.md).

; The OpenFOAM the flow studies are written for (oxyde-flow), pinned so that every install
; gets the same version; its installer's size is 203 083 004 bytes.
#define OpenFoamVersion "v2606"
#define OpenFoamFile "OpenFOAM-v2606-windows-mingw.exe"
#define OpenFoamUrl "https://dl.openfoam.com/source/v2606/OpenFOAM-v2606-windows-mingw.exe"
#define OpenFoamSha256 "020321d58b42e1d2ad0b616cdde17f92503608edd902388fb8c73e94e9cb1e25"

#ifndef AppVersion
  #define AppVersion "0.0.0"
#endif

[Setup]
; never change the AppId: Windows knows an upgrade from it
AppId={{B2E4F0A1-6C3D-4F58-9A7E-0D41C9E8A5F3}
AppName=Oxyde
AppVersion={#AppVersion}
AppVerName=Oxyde {#AppVersion}
AppPublisher=TheApache142857
AppPublisherURL=https://github.com/TheApache142857/oxyde
DefaultDirName={localappdata}\Programs\Oxyde
DefaultGroupName=Oxyde
DisableProgramGroupPage=yes
PrivilegesRequired=lowest
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
OutputDir=..\dist
OutputBaseFilename=Oxyde-Setup-{#AppVersion}
SetupIconFile=oxyde.ico
UninstallDisplayIcon={app}\Oxyde.exe
LicenseFile=..\LICENSE
Compression=lzma2
SolidCompression=yes
WizardStyle=modern
CloseApplications=yes
ChangesAssociations=yes

[Tasks]
Name: "desktopicon"; Description: "{cm:CreateDesktopIcon}"; GroupDescription: "{cm:AdditionalIcons}"; Flags: unchecked
Name: "associate"; Description: "Open .oxyprt and .oxyasm files with Oxyde"; GroupDescription: "Files:"
Name: "openfoam"; Description: "Download and install OpenFOAM {#OpenFoamVersion} from openfoam.com, for flow studies (203 MB; a separate program, GPL)"; GroupDescription: "Flow simulation:"; Flags: unchecked

[InstallDelete]
; the last version's DLLs: one it had and this one has not would stay
Type: files; Name: "{app}\*.dll"

[Files]
; Oxyde.exe, the OpenCASCADE DLLs and, when the build has the volume mesher, Netgen's
; (nglib.dll, ngcore.dll), the licences (THIRD-PARTY.txt) and the README
Source: "..\dist\Oxyde\*"; DestDir: "{app}"; Flags: ignoreversion recursesubdirs createallsubdirs

[Icons]
Name: "{autoprograms}\Oxyde"; Filename: "{app}\Oxyde.exe"
Name: "{autodesktop}\Oxyde"; Filename: "{app}\Oxyde.exe"; Tasks: desktopicon

[Registry]
; Per-user file types (HKCU), removed on uninstall.
Root: HKCU; Subkey: "Software\Classes\Oxyde.Part"; ValueType: string; ValueData: "Oxyde part"; Flags: uninsdeletekey; Tasks: associate
Root: HKCU; Subkey: "Software\Classes\Oxyde.Part\DefaultIcon"; ValueType: string; ValueData: "{app}\Oxyde.exe,0"; Tasks: associate
Root: HKCU; Subkey: "Software\Classes\Oxyde.Part\shell\open\command"; ValueType: string; ValueData: """{app}\Oxyde.exe"" ""%1"""; Tasks: associate
Root: HKCU; Subkey: "Software\Classes\Oxyde.Assembly"; ValueType: string; ValueData: "Oxyde assembly"; Flags: uninsdeletekey; Tasks: associate
Root: HKCU; Subkey: "Software\Classes\Oxyde.Assembly\DefaultIcon"; ValueType: string; ValueData: "{app}\Oxyde.exe,0"; Tasks: associate
Root: HKCU; Subkey: "Software\Classes\Oxyde.Assembly\shell\open\command"; ValueType: string; ValueData: """{app}\Oxyde.exe"" ""%1"""; Tasks: associate
Root: HKCU; Subkey: "Software\Classes\.oxyprt\OpenWithProgids"; ValueType: string; ValueName: "Oxyde.Part"; ValueData: ""; Flags: uninsdeletevalue; Tasks: associate
Root: HKCU; Subkey: "Software\Classes\.oxy\OpenWithProgids"; ValueType: string; ValueName: "Oxyde.Part"; ValueData: ""; Flags: uninsdeletevalue; Tasks: associate
Root: HKCU; Subkey: "Software\Classes\.oxyasm\OpenWithProgids"; ValueType: string; ValueName: "Oxyde.Assembly"; ValueData: ""; Flags: uninsdeletevalue; Tasks: associate
Root: HKCU; Subkey: "Software\Classes\.oxyprt"; ValueType: string; ValueData: "Oxyde.Part"; Flags: uninsdeletevalue; Tasks: associate
Root: HKCU; Subkey: "Software\Classes\.oxyasm"; ValueType: string; ValueData: "Oxyde.Assembly"; Flags: uninsdeletevalue; Tasks: associate

[Run]
; OpenFOAM's own installer, when it was downloaded; it asks for what it needs (shellexec
; lets it raise its rights).
Filename: "{tmp}\{#OpenFoamFile}"; StatusMsg: "Installing OpenFOAM {#OpenFoamVersion} (its own installer)..."; Flags: shellexec waituntilterminated skipifdoesntexist; Tasks: openfoam
Filename: "{app}\Oxyde.exe"; Description: "{cm:LaunchProgram,Oxyde}"; Flags: nowait postinstall skipifsilent

[Code]
var
  OpenFoamDownload: TDownloadWizardPage;

procedure InitializeWizard;
begin
  OpenFoamDownload := CreateDownloadPage(SetupMessage(msgWizardPreparing), 'Downloading OpenFOAM {#OpenFoamVersion} from openfoam.com', nil);
end;

// On Ready's Next, with the task picked: OpenFOAM's installer downloaded into {tmp} and
// checked. A failed download does not stop Oxyde's install; Preferences > Simulation
// says where to get OpenFOAM later.
function NextButtonClick(CurPageID: Integer): Boolean;
begin
  Result := True;
  if (CurPageID = wpReady) and WizardIsTaskSelected('openfoam') then
  begin
    OpenFoamDownload.Clear;
    OpenFoamDownload.Add('{#OpenFoamUrl}', '{#OpenFoamFile}', '{#OpenFoamSha256}');
    OpenFoamDownload.Show;
    try
      try
        OpenFoamDownload.Download;
      except
        if not OpenFoamDownload.AbortedByUser then
          SuppressibleMsgBox('OpenFOAM could not be downloaded: ' + GetExceptionMessage + #13#10#13#10 +
            'Oxyde installs without it. Flow studies need it: get OpenFOAM {#OpenFoamVersion} from https://www.openfoam.com/download later.',
            mbInformation, MB_OK, IDOK);
      end;
    finally
      OpenFoamDownload.Hide;
    end;
  end;
end;
