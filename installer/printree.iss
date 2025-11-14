[Setup]
AppName=printree
AppVersion={#MyAppVersion}
DefaultDirName={pf}\printree
DefaultGroupName=printree
OutputBaseFilename=printree_setup
Compression=lzma
SolidCompression=yes
ChangesEnvironment=yes

[Files]
Source: "target\x86_64-pc-windows-gnu\release\printree.exe"; DestDir: "{app}"; Flags: ignoreversion

[Icons]
Name: "{group}\printree"; Filename: "{app}\printree.exe"

[Run]
Filename: "{app}\printree.exe"; Description: "Run printree"; Flags: nowait postinstall skipifsilent

[Code]
const
  EnvironmentKey = 'SYSTEM\CurrentControlSet\Control\Session Manager\Environment';
  PathVariable = 'Path';

procedure AddInstallPathToSystemPath;
var
  CurrentPath: string;
begin
  if not RegQueryStringValue(HKEY_LOCAL_MACHINE, EnvironmentKey, PathVariable, CurrentPath) then
    CurrentPath := '';

  if Pos(';' + UpperCase(ExpandConstant('{app}')) + ';', ';' + UpperCase(CurrentPath) + ';') = 0 then
  begin
    if CurrentPath <> '' then
      CurrentPath := CurrentPath + ';';
    CurrentPath := CurrentPath + ExpandConstant('{app}');
    RegWriteStringValue(HKEY_LOCAL_MACHINE, EnvironmentKey, PathVariable, CurrentPath);
    Log(Format('Added "%s" to system PATH', [ExpandConstant('{app}')]));
  end
  else
    Log(Format('"%s" already present in system PATH', [ExpandConstant('{app}')]));
end;

procedure RemoveInstallPathFromSystemPath;
var
  CurrentPath, AppPath: string;
  Paths: TStringList;
  I: Integer;
  Updated: Boolean;
begin
  if not RegQueryStringValue(HKEY_LOCAL_MACHINE, EnvironmentKey, PathVariable, CurrentPath) then
    Exit;

  AppPath := UpperCase(ExpandConstant('{app}'));
  Paths := TStringList.Create;
  try
    Paths.StrictDelimiter := True;
    Paths.Delimiter := ';';
    Paths.DelimitedText := CurrentPath;
    Updated := False;

    I := Paths.Count - 1;
    while I >= 0 do
    begin
      if UpperCase(Trim(Paths[I])) = AppPath then
      begin
        Paths.Delete(I);
        Updated := True;
      end;
      I := I - 1;
    end;

    if Updated then
    begin
      RegWriteStringValue(HKEY_LOCAL_MACHINE, EnvironmentKey, PathVariable, Paths.DelimitedText);
      Log(Format('Removed "%s" from system PATH', [ExpandConstant('{app}')]));
    end
    else
      Log(Format('"%s" not present in system PATH', [ExpandConstant('{app}')]));
  finally
    Paths.Free;
  end;
end;

procedure CurStepChanged(CurStep: TSetupStep);
begin
  if CurStep = ssPostInstall then
    AddInstallPathToSystemPath;
end;

procedure CurUninstallStepChanged(CurUninstallStep: TUninstallStep);
begin
  if CurUninstallStep = usPostUninstall then
    RemoveInstallPathFromSystemPath;
end;
