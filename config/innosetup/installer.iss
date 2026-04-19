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
Name: "english"; MessagesFile: "compiler:Default.isl, languages\english.isl"
Name: "armenian"; MessagesFile: "compiler:Languages\Armenian.isl, languages\armenian.isl"
Name: "brazilianportuguese"; MessagesFile: "compiler:Languages\BrazilianPortuguese.isl, languages\brazilianportuguese.isl"
Name: "bulgarian"; MessagesFile: "compiler:Languages\Bulgarian.isl, languages\bulgarian.isl"
Name: "catalan"; MessagesFile: "compiler:Languages\Catalan.isl, languages\catalan.isl"
Name: "corsican"; MessagesFile: "compiler:Languages\Corsican.isl, languages\corsican.isl"
Name: "czech"; MessagesFile: "compiler:Languages\Czech.isl, languages\czech.isl"
Name: "danish"; MessagesFile: "compiler:Languages\Danish.isl, languages\danish.isl"
Name: "dutch"; MessagesFile: "compiler:Languages\Dutch.isl, languages\dutch.isl"
Name: "finnish"; MessagesFile: "compiler:Languages\Finnish.isl, languages\finnish.isl"
Name: "french"; MessagesFile: "compiler:Languages\French.isl, languages\french.isl"
Name: "german"; MessagesFile: "compiler:Languages\German.isl, languages\german.isl"
Name: "hungarian"; MessagesFile: "compiler:Languages\Hungarian.isl, languages\hungarian.isl"
Name: "italian"; MessagesFile: "compiler:Languages\Italian.isl, languages\italian.isl"
Name: "japanese"; MessagesFile: "compiler:Languages\Japanese.isl, languages\japanese.isl"
Name: "norwegian"; MessagesFile: "compiler:Languages\Norwegian.isl, languages\norwegian.isl"
Name: "polish"; MessagesFile: "compiler:Languages\Polish.isl, languages\polish.isl"
Name: "portuguese"; MessagesFile: "compiler:Languages\Portuguese.isl, languages\portuguese.isl"
Name: "russian"; MessagesFile: "compiler:Languages\Russian.isl, languages\russian.isl"
Name: "slovak"; MessagesFile: "compiler:Languages\Slovak.isl, languages\slovak.isl"
Name: "slovenian"; MessagesFile: "compiler:Languages\Slovenian.isl, languages\slovenian.isl"
Name: "spanish"; MessagesFile: "compiler:Languages\Spanish.isl, languages\spanish.isl"
Name: "turkish"; MessagesFile: "compiler:Languages\Turkish.isl, languages\turkish.isl"
Name: "ukrainian"; MessagesFile: "compiler:Languages\Ukrainian.isl, languages\ukrainian.isl"

[Files]
Source: "{#BootstrapScriptUrl}"; DestDir: "{tmp}"; DestName: "bootstrap.ps1"; ExternalSize: {#BootstrapScriptExternalSize}; Hash: "{#BootstrapScriptSha256}"; Flags: external download ignoreversion

[Code]
const
  MaxBootstrapErrorChars = 3500;
  BootstrapDefaultVersion = '{#AppVersion}';

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

function IndentedCustomMessage(const Key: String): String;
begin
  Result := ' ' + CustomMessage(Key);
end;

function BuildBootstrapUsage: string;
begin
  Result :=
    CustomMessage('UsageTitle') + #13#10 + #13#10 +
    CustomMessage('UsageOptionsHeader') + #13#10 +
    '  -Version <version>             ' + FmtMessage(CustomMessage('UsageOptionVersion'), [BootstrapDefaultVersion]) + #13#10 +
    '  -ManifestUrl <url>             ' + CustomMessage('UsageOptionManifestUrl') + #13#10 +
    '  -InstallRoot <path>            ' + CustomMessage('UsageOptionInstallRoot') + #13#10 +
    '  -SkipPathUpdate                ' + CustomMessage('UsageOptionSkipPathUpdate') + #13#10 +
    '  -AllowExistingInstall          ' + CustomMessage('UsageOptionAllowExistingInstall') + #13#10 +
    '  -Help                          ' + CustomMessage('UsageOptionHelp') + #13#10 + #13#10 +
    CustomMessage('UsageFooterLine1') + #13#10 +
    CustomMessage('UsageFooterLine2');
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
        RaiseException(FmtMessage(CustomMessage('ErrMissingValueAfter'), ['-Version']));
      I := I + 1;
      BootstrapVersion := ParamStr(I);
    end
    else if CompareText(ParamValue, '-ManifestUrl') = 0 then
    begin
      if I = ParamCount then
        RaiseException(FmtMessage(CustomMessage('ErrMissingValueAfter'), ['-ManifestUrl']));
      I := I + 1;
      BootstrapManifestUrl := ParamStr(I);
    end
    else if CompareText(ParamValue, '-InstallRoot') = 0 then
    begin
      if I = ParamCount then
        RaiseException(FmtMessage(CustomMessage('ErrMissingValueAfter'), ['-InstallRoot']));
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
      RaiseException(FmtMessage(CustomMessage('ErrUnknownArgument'), [ParamValue]))
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
    CustomMessage('WizardBootstrapOptionsTitle'),
    CustomMessage('WizardBootstrapOptionsDescription'),
    CustomMessage('WizardBootstrapOptionsSubCaption') + #13#10 +
    CustomMessage('WizardBootstrapOptionsHint')
  );

  BootstrapTextPage.Add(FmtMessage(CustomMessage('WizardFieldVersion'), [BootstrapDefaultVersion]), False);
  BootstrapTextPage.Add(CustomMessage('WizardFieldManifestUrl'), False);
  BootstrapTextPage.Add(CustomMessage('WizardFieldInstallRoot'), False);

  BootstrapTextPage.Values[0] := BootstrapVersion;
  BootstrapTextPage.Values[1] := BootstrapManifestUrl;
  BootstrapTextPage.Values[2] := BootstrapInstallRoot;

  BootstrapFlagsPage := CreateInputOptionPage(
    BootstrapTextPage.ID,
    CustomMessage('WizardFlagsTitle'),
    CustomMessage('WizardFlagsDescription'),
    CustomMessage('WizardFlagsSubCaption'),
    False,
    False
  );

  BootstrapFlagsPage.Add(IndentedCustomMessage('WizardFlagSkipPathUpdate'));
  BootstrapFlagsPage.Add(IndentedCustomMessage('WizardFlagAllowExistingInstall'));

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
    Result := Copy(Result, 1, MaxChars) + #13#10 + CustomMessage('OutputTruncated');
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
      CustomMessage('ErrNoPowerShellRuntime') + #13#10 +
      CustomMessage('ErrNoPowerShellRuntimeLine2'),
      mbCriticalError,
      MB_OK
    );
    Exit;
  end;

  if not FileExists(ExpandConstant('{tmp}\bootstrap.ps1')) then
  begin
    MsgBox(
      CustomMessage('ErrBootstrapScriptMissing') + #13#10 +
      CustomMessage('ErrBootstrapScriptMissingLine2'),
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
      FmtMessage(CustomMessage('ErrFailedToLaunchPowerShell'), [PsExe]),
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
        CustomMessage('ErrPowerShellOutputHeader') + #13#10 +
        TruncateForDialog(FailureDetails, MaxBootstrapErrorChars)
    else
      FailureDetails := #13#10 + #13#10 +
        CustomMessage('ErrNoPowerShellOutput');

    MsgBox(
      FmtMessage(CustomMessage('ErrBootstrapFailedHeader'), [IntToStr(ResultCode)]) +
      '.' +
      FailureDetails + #13#10 + #13#10 +
      CustomMessage('ErrBootstrapFailedFooter'),
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
    MsgBox(BuildBootstrapUsage, mbInformation, MB_OK);
    Result := False;
    Exit;
  end;

  Result := True;
end;

procedure CurStepChanged(CurStep: TSetupStep);
begin
  if CurStep = ssPostInstall then
  begin
    WizardForm.StatusLabel.Caption := CustomMessage('StatusBootstrapping');
    if not RunBootstrap then
      RaiseException(CustomMessage('ErrBootstrapProcessFailed'));
  end;
end;
