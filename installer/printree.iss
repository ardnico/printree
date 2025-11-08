[Setup]
AppName=printree
AppVersion={#MyAppVersion}
DefaultDirName={pf}\printree
DefaultGroupName=printree
OutputBaseFilename=printree_setup
Compression=lzma
SolidCompression=yes

[Files]
Source: "target\x86_64-pc-windows-gnu\release\printree.exe"; DestDir: "{app}"; Flags: ignoreversion

[Icons]
Name: "{group}\printree"; Filename: "{app}\printree.exe"

[Run]
Filename: "{app}\printree.exe"; Description: "Run printree"; Flags: nowait postinstall skipifsilent
