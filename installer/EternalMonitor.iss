; EternalMonitor host installer (Inno Setup 6)
; Builds a single EternalMonitor-Setup.exe that installs the host app, the FFmpeg
; runtime, and (optionally) bundles + installs the Virtual Display Driver so the iPad
; can act as an extended display with no manual driver steps.
;
; Do not compile this directly — run scripts\build-installer.ps1, which stages the
; files and passes the required /D defines below:
;   StagingDir  : absolute path to the staged payload (app exe, DLLs, docs, driver\)
;   AppVersion  : version string, e.g. 0.1.1
;   IncludeDriver (optional, defined only when a driver setup .exe was staged)

#ifndef StagingDir
  #error "StagingDir is not defined — run scripts\build-installer.ps1 instead of compiling the .iss directly."
#endif
#ifndef AppVersion
  #define AppVersion "0.0.0"
#endif

[Setup]
AppId={{B7E6F2C4-3A91-4E0D-9C2A-ETERNALMONITOR}}
AppName=EternalMonitor
AppVersion={#AppVersion}
AppVerName=EternalMonitor {#AppVersion}
AppPublisher=Ali Younes
AppPublisherURL=https://github.com/whoisaldo/EternalMonitor
AppSupportURL=https://github.com/whoisaldo/EternalMonitor
AppUpdatesURL=https://github.com/whoisaldo/EternalMonitor/releases
AppContact=aliyounes@eternalreverse.com
DefaultDirName={autopf}\EternalMonitor
DefaultGroupName=EternalMonitor
DisableProgramGroupPage=yes
DisableDirPage=yes
; Driver installation requires elevation — request it once for the whole run.
PrivilegesRequired=admin
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
OutputDir={#StagingDir}\..\out
OutputBaseFilename=EternalMonitor-Setup
Compression=lzma2/max
SolidCompression=yes
; --- Branding (SIGNAL look) ---------------------------------------------------
WizardStyle=modern
SetupIconFile=..\host\assets\icon.ico
WizardImageFile=assets\wizard-large.bmp
WizardSmallImageFile=assets\wizard-small.bmp
WizardImageStretch=yes
UninstallDisplayName=EternalMonitor
UninstallDisplayIcon={app}\EternalMonitor-host.exe
SetupLogging=yes

[Languages]
Name: "english"; MessagesFile: "compiler:Default.isl"

[Tasks]
Name: "desktopicon"; Description: "Create a desktop shortcut"; GroupDescription: "Shortcuts:"
; Autostart is handled inside the app (Settings -> "Start on Windows startup",
; which writes HKCU\Run for the signed-in user) — no installer task needed.

[Files]
Source: "{#StagingDir}\EternalMonitor-host.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#StagingDir}\*.dll";                   DestDir: "{app}"; Flags: ignoreversion
Source: "{#StagingDir}\ffmpeg.exe";              DestDir: "{app}"; Flags: ignoreversion skipifsourcedoesntexist
Source: "{#StagingDir}\README.md";               DestDir: "{app}"; Flags: ignoreversion isreadme skipifsourcedoesntexist
Source: "{#StagingDir}\LICENSE";                 DestDir: "{app}"; Flags: ignoreversion skipifsourcedoesntexist
Source: "{#StagingDir}\QUICKSTART.txt";          DestDir: "{app}"; Flags: ignoreversion skipifsourcedoesntexist
#ifdef IncludeDriver
; Third-party Virtual Display Driver setup, bundled so the tester never touches GitHub.
Source: "{#StagingDir}\driver\*"; DestDir: "{app}\driver"; Flags: ignoreversion recursesubdirs
; Scripts that register/remove the scheduled tasks the host uses to toggle the display.
Source: "scripts\vdd-tasks-setup.ps1";  DestDir: "{app}\scripts"; Flags: ignoreversion
Source: "scripts\vdd-tasks-remove.ps1"; DestDir: "{app}\scripts"; Flags: ignoreversion
#endif

[Icons]
Name: "{group}\EternalMonitor";            Filename: "{app}\EternalMonitor-host.exe"
Name: "{group}\Quickstart (read me)";      Filename: "{app}\QUICKSTART.txt"
Name: "{group}\Uninstall EternalMonitor";  Filename: "{uninstallexe}"
Name: "{autodesktop}\EternalMonitor";      Filename: "{app}\EternalMonitor-host.exe"; Tasks: desktopicon

[Run]
#ifdef IncludeDriver
; Install the virtual display driver during setup. We're already elevated, so this
; runs without a second UAC prompt. /VERYSILENT works when the bundled setup is
; Inno-based; non-Inno installers ignore it and show their own short wizard instead.
Filename: "{app}\driver\vdd-setup-x64.exe"; Parameters: "/VERYSILENT /SUPPRESSMSGBOXES /NORESTART"; StatusMsg: "Installing the virtual display driver (this enables the extended screen)..."; Flags: waituntilterminated
; Register the enable/disable scheduled tasks and leave the virtual display OFF by default —
; EternalMonitor turns it on only while streaming to it, so there's no phantom monitor.
Filename: "powershell.exe"; Parameters: "-NoProfile -ExecutionPolicy Bypass -File ""{app}\scripts\vdd-tasks-setup.ps1"""; StatusMsg: "Configuring the on-demand virtual display..."; Flags: runhidden waituntilterminated
#endif
; Launch the app at the end.
Filename: "{app}\EternalMonitor-host.exe"; Description: "Launch EternalMonitor now"; Flags: nowait postinstall skipifsilent

[UninstallRun]
#ifdef IncludeDriver
; Remove the scheduled tasks and re-enable the device before removing the driver.
Filename: "powershell.exe"; Parameters: "-NoProfile -ExecutionPolicy Bypass -File ""{app}\scripts\vdd-tasks-remove.ps1"""; Flags: runhidden; RunOnceId: "VddTasksRemove"
; Best-effort driver uninstall via the bundled setup's uninstaller, if present.
Filename: "{app}\driver\vdd-setup-x64.exe"; Parameters: "/VERYSILENT /SUPPRESSMSGBOXES /NORESTART /uninstall"; Flags: waituntilterminated; RunOnceId: "VddUninstall"
#endif
