; Fidan bootstrap installer wrapper (Inno Setup)
; This installer downloads and executes https://fidan.dev/install.ps1 so bootstrap behavior stays aligned.

#define AppName "Fidan"
#define AppPublisher "Kaan Gönüldinc (AppSolves)"
#define AppURL "https://fidan.dev"
#define BootstrapScriptUrl "https://fidan.dev/install.ps1"
#define BootstrapScriptExternalSize GetEnv('BOOTSTRAP_SCRIPT_SIZE')
#if BootstrapScriptExternalSize == ""
  #error "BOOTSTRAP_SCRIPT_SIZE environment variable is not set. Please set it to the size of the bootstrap script in bytes."
#endif
#define BootstrapScriptSha256 GetEnv('BOOTSTRAP_SCRIPT_SHA256')
#if BootstrapScriptSha256 == ""
  #error "BOOTSTRAP_SCRIPT_SHA256 environment variable is not set. Please set it to the SHA256 hash of the bootstrap script."
#endif
#define AppVersion GetEnv('VERSION')
#if AppVersion == ""
  #error "VERSION environment variable is not set. Please set it to the version of your application."
#endif
#define ROOT_DIR GetEnv('ROOT_DIR')
#if ROOT_DIR == ""
  #error "ROOT_DIR environment variable is not set. Please set it to the root directory of your project."
#endif

[Setup]
SignTool=CertForge $f
SignToolRunMinimized=true
SignedUninstaller=no
AppId={{99F4E202-989A-4413-BDCC-629B70BD1AB3}
AppName={#AppName} Bootstrap Installer
AppVersion={#AppVersion}
AppPublisher={#AppPublisher}
AppPublisherURL={#AppURL}
AppSupportURL={#AppURL}
AppUpdatesURL={#AppURL}
AppCopyright=Copyright (C) 2026 {#AppPublisher}. All rights reserved.
LicenseFile={#ROOT_DIR}\LICENSE
SetupIconFile={#ROOT_DIR}\assets\icons\installer.ico
DefaultDirName={localappdata}\Programs\{#AppName}
DisableDirPage=yes
DisableProgramGroupPage=yes
DisableReadyMemo=yes
CreateAppDir=no
Uninstallable=no
PrivilegesRequired=lowest
OutputDir={#ROOT_DIR}\dist\innosetup\installers
OutputBaseFilename=fidan_windows_bootstrap_v{#AppVersion}
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
AllowCancelDuringInstall=no
ChangesEnvironment=yes
Compression=lzma2
SolidCompression=yes
WizardStyle=modern dynamic

[Languages]
Name: "english"; MessagesFile: "compiler:Default.isl"
Name: "armenian"; MessagesFile: "compiler:Languages\Armenian.isl"
Name: "brazilianportuguese"; MessagesFile: "compiler:Languages\BrazilianPortuguese.isl"
Name: "bulgarian"; MessagesFile: "compiler:Languages\Bulgarian.isl"
Name: "catalan"; MessagesFile: "compiler:Languages\Catalan.isl"
Name: "corsican"; MessagesFile: "compiler:Languages\Corsican.isl"
Name: "czech"; MessagesFile: "compiler:Languages\Czech.isl"
Name: "danish"; MessagesFile: "compiler:Languages\Danish.isl"
Name: "dutch"; MessagesFile: "compiler:Languages\Dutch.isl"
Name: "finnish"; MessagesFile: "compiler:Languages\Finnish.isl"
Name: "french"; MessagesFile: "compiler:Languages\French.isl"
Name: "german"; MessagesFile: "compiler:Languages\German.isl"
Name: "hebrew"; MessagesFile: "compiler:Languages\Hebrew.isl"
Name: "hungarian"; MessagesFile: "compiler:Languages\Hungarian.isl"
Name: "italian"; MessagesFile: "compiler:Languages\Italian.isl"
Name: "japanese"; MessagesFile: "compiler:Languages\Japanese.isl"
Name: "norwegian"; MessagesFile: "compiler:Languages\Norwegian.isl"
Name: "polish"; MessagesFile: "compiler:Languages\Polish.isl"
Name: "portuguese"; MessagesFile: "compiler:Languages\Portuguese.isl"
Name: "russian"; MessagesFile: "compiler:Languages\Russian.isl"
Name: "slovak"; MessagesFile: "compiler:Languages\Slovak.isl"
Name: "slovenian"; MessagesFile: "compiler:Languages\Slovenian.isl"
Name: "spanish"; MessagesFile: "compiler:Languages\Spanish.isl"
Name: "turkish"; MessagesFile: "compiler:Languages\Turkish.isl"
Name: "ukrainian"; MessagesFile: "compiler:Languages\Ukrainian.isl"

[Files]
Source: "{#BootstrapScriptUrl}"; DestDir: "{tmp}"; DestName: "bootstrap.ps1"; ExternalSize: {#BootstrapScriptExternalSize}; Hash: "{#BootstrapScriptSha256}"; Flags: external download ignoreversion

[Code]
const
  MaxBootstrapErrorChars = 3500;
  BootstrapDefaultVersion = '{#AppVersion}';
  BootstrapUsage =
    'Fidan bootstrap installer' + #13#10 + #13#10 +
    'Options:' + #13#10 +
    '  -Version <version>             Install a specific released version (default: ' + BootstrapDefaultVersion + '; use "latest" for newest published release)' + #13#10 +
    '  -ManifestUrl <url>             Override the distribution manifest URL' + #13#10 +
    '  -InstallRoot <path>            Override the self-managed install root' + #13#10 +
    '  -SkipPathUpdate                Do not modify the user PATH' + #13#10 +
    '  -AllowExistingInstall          Permit bootstrapping into an existing Fidan install root' + #13#10 +
    '  -Help                          Show this help text' + #13#10 + #13#10 +
    'Bootstrap is intended for first install. If Fidan is already installed,' + #13#10 +
    'prefer "fidan self install" and "fidan self use".';

var
  BootstrapVersion: string;
  BootstrapManifestUrl: string;
  BootstrapInstallRoot: string;
  BootstrapSkipPathUpdate: Boolean;
  BootstrapAllowExistingInstall: Boolean;
  BootstrapHelp: Boolean;
  BootstrapTextPage: TInputQueryWizardPage;
  BootstrapFlagsPage: TInputOptionWizardPage;

function StartsTextIgnoreCase(const Prefix, Value: string): Boolean;
begin
  Result := CompareText(Copy(Value, 1, Length(Prefix)), Prefix) = 0;
end;

procedure ParseBootstrapParams;
var
  I: Integer;
  ParamValue: string;
  UpperParam: string;
begin
  BootstrapVersion := BootstrapDefaultVersion;
  BootstrapManifestUrl := '';
  BootstrapInstallRoot := '';
  BootstrapSkipPathUpdate := False;
  BootstrapAllowExistingInstall := False;
  BootstrapHelp := False;

  I := 1;
  while I <= ParamCount do
  begin
    ParamValue := ParamStr(I);
    UpperParam := UpperCase(ParamValue);

    if CompareText(ParamValue, '-Version') = 0 then
    begin
      if I = ParamCount then
        RaiseException('Missing value after -Version');
      I := I + 1;
      BootstrapVersion := ParamStr(I);
    end
    else if CompareText(ParamValue, '-ManifestUrl') = 0 then
    begin
      if I = ParamCount then
        RaiseException('Missing value after -ManifestUrl');
      I := I + 1;
      BootstrapManifestUrl := ParamStr(I);
    end
    else if CompareText(ParamValue, '-InstallRoot') = 0 then
    begin
      if I = ParamCount then
        RaiseException('Missing value after -InstallRoot');
      I := I + 1;
      BootstrapInstallRoot := ParamStr(I);
    end
    else if CompareText(ParamValue, '-SkipPathUpdate') = 0 then
      BootstrapSkipPathUpdate := True
    else if CompareText(ParamValue, '-AllowExistingInstall') = 0 then
      BootstrapAllowExistingInstall := True
    else if CompareText(ParamValue, '-Help') = 0 then
      BootstrapHelp := True
    else if (Length(ParamValue) > 0) and (ParamValue[1] = '-') then
      RaiseException('unknown argument: ' + ParamValue)
    else if StartsTextIgnoreCase('/VERSION=', UpperParam) then
      BootstrapVersion := Copy(ParamValue, Length('/VERSION=') + 1, MaxInt)
    else if StartsTextIgnoreCase('/MANIFESTURL=', UpperParam) then
      BootstrapManifestUrl := Copy(ParamValue, Length('/MANIFESTURL=') + 1, MaxInt)
    else if StartsTextIgnoreCase('/INSTALLROOT=', UpperParam) then
      BootstrapInstallRoot := Copy(ParamValue, Length('/INSTALLROOT=') + 1, MaxInt)
    else if CompareText(UpperParam, '/SKIPPATHUPDATE') = 0 then
      BootstrapSkipPathUpdate := True
    else if CompareText(UpperParam, '/ALLOWEXISTINGINSTALL') = 0 then
      BootstrapAllowExistingInstall := True
    else if (CompareText(UpperParam, '/HELP') = 0) or (CompareText(UpperParam, '/?') = 0) then
      BootstrapHelp := True;

    I := I + 1;
  end;
end;

procedure ApplyInteractiveBootstrapParams;
begin
  if WizardSilent then
    Exit;

  if Assigned(BootstrapTextPage) then
  begin
    BootstrapVersion := Trim(BootstrapTextPage.Values[0]);
    if BootstrapVersion = '' then
      BootstrapVersion := BootstrapDefaultVersion;

    BootstrapManifestUrl := Trim(BootstrapTextPage.Values[1]);
    BootstrapInstallRoot := Trim(BootstrapTextPage.Values[2]);
  end;

  if Assigned(BootstrapFlagsPage) then
  begin
    BootstrapSkipPathUpdate := BootstrapFlagsPage.Values[0];
    BootstrapAllowExistingInstall := BootstrapFlagsPage.Values[1];
  end;
end;

procedure InitializeWizard;
begin
  if WizardSilent then
    Exit;

  BootstrapTextPage := CreateInputQueryPage(
    wpWelcome,
    'Bootstrap Options',
    'Configure bootstrap parameters',
    'These options are equivalent to bootstrap command-line arguments.' + #13#10 +
    'Leave Manifest URL and Install root empty to use bootstrap defaults.'
  );

  BootstrapTextPage.Add('Version (default: ' + BootstrapDefaultVersion + ', or "latest")', False);
  BootstrapTextPage.Add('Manifest URL override (optional):', False);
  BootstrapTextPage.Add('Install root override (optional):', False);

  BootstrapTextPage.Values[0] := BootstrapVersion;
  BootstrapTextPage.Values[1] := BootstrapManifestUrl;
  BootstrapTextPage.Values[2] := BootstrapInstallRoot;

  BootstrapFlagsPage := CreateInputOptionPage(
    BootstrapTextPage.ID,
    'Bootstrap Flags',
    'Choose optional bootstrap flags',
    'These map directly to bootstrap command-line switch behavior.',
    False,
    False
  );

  BootstrapFlagsPage.Add(' Skip PATH update');
  BootstrapFlagsPage.Add(' Allow existing install root');

  BootstrapFlagsPage.Values[0] := BootstrapSkipPathUpdate;
  BootstrapFlagsPage.Values[1] := BootstrapAllowExistingInstall;
end;

function BuildBootstrapPowerShellArgs: string;
begin
  Result :=
    '-NoProfile -ExecutionPolicy Bypass -File ' +
    AddQuotes(ExpandConstant('{tmp}\bootstrap.ps1'));

  if BootstrapVersion <> '' then
    Result := Result + ' -Version ' + AddQuotes(BootstrapVersion);
  if BootstrapManifestUrl <> '' then
    Result := Result + ' -ManifestUrl ' + AddQuotes(BootstrapManifestUrl);
  if BootstrapInstallRoot <> '' then
    Result := Result + ' -InstallRoot ' + AddQuotes(BootstrapInstallRoot);
  if BootstrapSkipPathUpdate then
    Result := Result + ' -SkipPathUpdate';
  if BootstrapAllowExistingInstall then
    Result := Result + ' -AllowExistingInstall';
end;

function TruncateForDialog(const Value: string; MaxChars: Integer): string;
begin
  Result := Value;
  if Length(Result) > MaxChars then
    Result := Copy(Result, 1, MaxChars) + #13#10 + '... (output truncated)';
end;

function NormalizeBootstrapOutput(const Value: string): string;
var
  MarkerPos: Integer;
begin
  Result := Trim(Value);
  if Result = '' then
    Exit;

  MarkerPos := Pos('[X]', Result);
  if MarkerPos = 0 then
    MarkerPos := Pos('Installation failed:', Result);
  if MarkerPos = 0 then
    MarkerPos := Pos('Use "fidan self install"', Result);

  if MarkerPos > 0 then
    Result := Trim(Copy(Result, MarkerPos, MaxInt));
end;

function ReadBootstrapOutput(const OutputPath: string): string;
var
  OutputText: AnsiString;
begin
  Result := '';
  if LoadStringFromFile(OutputPath, OutputText) then
  begin
    Result := NormalizeBootstrapOutput(String(OutputText));
  end;
end;

function ResolvePowerShellExe: string;
var
  ResultCode: Integer;
begin
  Result := '';

  if Exec(
    ExpandConstant('{cmd}'),
    '/C where pwsh.exe >nul 2>nul',
    '',
    SW_HIDE,
    ewWaitUntilTerminated,
    ResultCode
  ) and (ResultCode = 0) then
  begin
    Result := 'pwsh.exe';
    Exit;
  end;

  if Exec(
    ExpandConstant('{cmd}'),
    '/C where powershell.exe >nul 2>nul',
    '',
    SW_HIDE,
    ewWaitUntilTerminated,
    ResultCode
  ) and (ResultCode = 0) then
  begin
    Result := 'powershell.exe';
    Exit;
  end;
end;

function RunBootstrap: Boolean;
var
  ResultCode: Integer;
  ShowCmd: Integer;
  CmdArgs: string;
  PsArgs: string;
  PsExe: string;
  PsOutputPath: string;
  FailureDetails: string;
begin
  Result := False;

  ApplyInteractiveBootstrapParams;
  PsOutputPath := ExpandConstant('{tmp}\bootstrap-output.log');
  if FileExists(PsOutputPath) then
    DeleteFile(PsOutputPath);

  if WizardSilent then
    ShowCmd := SW_HIDE
  else
    ShowCmd := SW_SHOWMINNOACTIVE;

  PsExe := ResolvePowerShellExe();
  if PsExe = '' then
  begin
    MsgBox(
      'No supported PowerShell runtime was found.' + #13#10 +
      'Install PowerShell (pwsh) or ensure Windows PowerShell is available, then rerun setup.',
      mbCriticalError,
      MB_OK
    );
    Exit;
  end;

  if not FileExists(ExpandConstant('{tmp}\bootstrap.ps1')) then
  begin
    MsgBox(
      'Downloaded bootstrap script was not found in temporary directory.' + #13#10 +
      'Please retry setup. If the issue persists, check network/proxy settings.',
      mbCriticalError,
      MB_OK
    );
    Exit;
  end;

  PsArgs := BuildBootstrapPowerShellArgs();
  CmdArgs :=
    '/C ' +
    AddQuotes(AddQuotes(PsExe) + ' ' + PsArgs + ' > ' + AddQuotes(PsOutputPath) + ' 2>&1');

  Log('Running bootstrap via cmd with engine ' + PsExe + ' and arguments: ' + PsArgs);

  if not Exec(ExpandConstant('{cmd}'), CmdArgs, '', ShowCmd, ewWaitUntilTerminated, ResultCode) then
  begin
    MsgBox(
      'Failed to launch ' + PsExe + ' to run bootstrap.ps1.',
      mbCriticalError,
      MB_OK
    );
    Exit;
  end;

  if ResultCode <> 0 then
  begin
    FailureDetails := ReadBootstrapOutput(PsOutputPath);
    if FailureDetails <> '' then
      FailureDetails := #13#10 + #13#10 +
        'PowerShell output:' + #13#10 +
        TruncateForDialog(FailureDetails, MaxBootstrapErrorChars)
    else
      FailureDetails := #13#10 + #13#10 +
        'No PowerShell output was captured.';

    MsgBox(
      'Fidan bootstrap failed with exit code ' + IntToStr(ResultCode) +
      '.' +
      FailureDetails + #13#10 + #13#10 +
      'If needed, rerun with logging enabled and check the installer log.',
      mbCriticalError,
      MB_OK
    );
    Exit;
  end;

  Result := True;
end;

function InitializeSetup: Boolean;
begin
  ParseBootstrapParams;

  if BootstrapHelp then
  begin
    MsgBox(BootstrapUsage, mbInformation, MB_OK);
    Result := False;
    Exit;
  end;

  Result := True;
end;

procedure CurStepChanged(CurStep: TSetupStep);
begin
  if CurStep = ssPostInstall then
  begin
    WizardForm.StatusLabel.Caption := 'Bootstrapping Fidan...';
    if not RunBootstrap then
      RaiseException('Bootstrap process failed.');
  end;
end;
