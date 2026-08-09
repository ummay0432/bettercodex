#Requires -Version 5.1

[CmdletBinding()]
param()

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

$RepositoryRoot = Split-Path -Parent (Split-Path -Parent $MyInvocation.MyCommand.Path)
$InstallerPath = Join-Path $RepositoryRoot 'scripts\install.ps1'
$InstallerText = [IO.File]::ReadAllText($InstallerPath)
$Tokens = $null
$ParseErrors = $null
$Ast = [Management.Automation.Language.Parser]::ParseFile(
    $InstallerPath,
    [ref]$Tokens,
    [ref]$ParseErrors
)
if ($ParseErrors.Count -ne 0) {
    throw "install.ps1 has parser errors: $($ParseErrors.Message -join '; ')"
}

foreach ($FunctionAst in $Ast.FindAll({
            param($Node)
            $Node -is [Management.Automation.Language.FunctionDefinitionAst]
        }, $true)) {
    Invoke-Expression $FunctionAst.Extent.Text
}

$global:MaximumArchiveBytes = 128MB
$global:MaximumBinaryBytes = 256MB
$Revision = '1' * 40
$Tag = "bcodex-v1.2.3-$Revision"
$TestRoot = Join-Path ([IO.Path]::GetTempPath()) (
    'bettercodex-windows-installer-tests.' + [Guid]::NewGuid().ToString('N')
)
[void](New-Item -ItemType Directory -Path $TestRoot)

function Assert-True([bool] $Condition, [string] $Message) {
    if (-not $Condition) { throw $Message }
}

function Assert-Equal($Actual, $Expected, [string] $Message) {
    if ($Actual -cne $Expected) {
        throw "$Message (actual: '$Actual', expected: '$Expected')"
    }
}

function Write-Candidate(
    [string] $Path,
    [string] $ReleaseTag,
    [string] $Version,
    [bool] $SmokeSucceeds = $true
) {
    $SmokeExit = if ($SmokeSucceeds) { 'exit /b 0' } else { 'exit /b 9' }
    $Source = @"
@echo off
if "%~1"=="--internal-release-tag" (
  echo $ReleaseTag
  exit /b 0
)
if "%~1"=="--version" (
  echo bcodex $Version
  exit /b 0
)
if "%~1"=="--internal-install-smoke" (
  echo bcodex $Version install smoke passed
  $SmokeExit
)
exit /b 64
"@
    [IO.File]::WriteAllText($Path, $Source, [Text.Encoding]::ASCII)
}

function Compress-Gzip([string] $Source, [string] $Destination) {
    $SourceStream = [IO.File]::OpenRead($Source)
    $ArchiveStream = [IO.File]::Create($Destination)
    $Gzip = New-Object IO.Compression.GZipStream(
        $ArchiveStream,
        [IO.Compression.CompressionMode]::Compress,
        $false
    )
    try {
        $SourceStream.CopyTo($Gzip)
    }
    finally {
        $Gzip.Dispose()
        $ArchiveStream.Dispose()
        $SourceStream.Dispose()
    }
}

try {
    Assert-True (Test-ReleaseTag $Tag) 'valid release tag was rejected'
    Assert-True (-not (Test-ReleaseTag 'v1.2.3')) 'invalid release tag was accepted'
    Assert-True (-not (Test-ReleaseTag "bcodex-v1.2-$Revision")) 'short version was accepted'
    Assert-Equal (Get-ReleaseVersion $Tag) '1.2.3' 'release version parsing failed'

    $Path = Prepend-PathEntry 'C:\Existing;D:\Tools' 'C:\bettercodex\bin'
    Assert-Equal $Path 'C:\bettercodex\bin;C:\Existing;D:\Tools' 'PATH prepend failed'
    $Path = Prepend-PathEntry 'c:\BETTERCODEX\BIN;D:\Tools' 'C:\bettercodex\bin'
    Assert-Equal $Path 'C:\bettercodex\bin;D:\Tools' 'PATH deduplication failed'

    $Source = Join-Path $TestRoot 'source.bin'
    $Archive = Join-Path $TestRoot 'source.bin.gz'
    $Expanded = Join-Path $TestRoot 'expanded.bin'
    [IO.File]::WriteAllBytes($Source, [Text.Encoding]::UTF8.GetBytes(('payload-' * 4096)))
    Compress-Gzip $Source $Archive
    Expand-GzipBinary $Archive $Expanded
    Assert-Equal (Get-FileSha256 $Expanded) (Get-FileSha256 $Source) 'gzip expansion changed bytes'

    $Candidate = Join-Path $TestRoot 'candidate.cmd'
    Write-Candidate $Candidate $Tag '1.2.3'
    Assert-Equal (Get-BinaryReleaseTag $Candidate) $Tag 'candidate tag verification failed'
    Assert-True (Test-BinaryIdentity $Candidate $Tag '1.2.3') 'candidate identity failed'
    Assert-True (-not (Test-BinaryIdentity $Candidate $Tag '9.9.9')) 'wrong version was accepted'
    Invoke-BinarySmoke $Candidate '1.2.3'

    $Destination = Join-Path $TestRoot 'installed.cmd'
    $Backup = Join-Path $TestRoot 'backup.cmd'
    Write-Candidate $Destination "bcodex-v1.2.2-$('2' * 40)" '1.2.2'
    Install-Candidate $Candidate $Destination $Tag '1.2.3' $Backup
    Assert-True (Test-BinaryIdentity $Destination $Tag '1.2.3') 'candidate was not installed'
    Assert-True (-not (Test-Path -LiteralPath $Backup)) 'successful install retained backup'

    $OldDigest = Get-FileSha256 $Destination
    $InvalidCandidate = Join-Path $TestRoot 'invalid-candidate.cmd'
    Write-Candidate $InvalidCandidate "bcodex-v9.9.9-$('9' * 40)" '9.9.9'
    $RollbackFailed = $false
    try {
        Install-Candidate $InvalidCandidate $Destination $Tag '1.2.3' $Backup
    }
    catch {
        $RollbackFailed = $true
    }
    Assert-True $RollbackFailed 'invalid replacement unexpectedly succeeded'
    Assert-Equal (Get-FileSha256 $Destination) $OldDigest 'failed replacement did not roll back'

    $OriginalLocalAppData = $env:LOCALAPPDATA
    try {
        $env:LOCALAPPDATA = Join-Path $TestRoot 'local-app-data'
        $LegacyCache = Join-Path $env:LOCALAPPDATA 'bettercodex\cache'
        foreach ($Name in @('build', 'cargo', 'rustup', 'tmp', 'downloads', 'rusty-v8-obsolete')) {
            [void](New-Item -ItemType Directory -Force -Path (Join-Path $LegacyCache $Name))
        }
        $PrivatePath = Join-Path $TestRoot 'bin\bcodex-path'
        [void](New-Item -ItemType Directory -Force -Path $PrivatePath)
        Remove-LegacyInstallState (Split-Path -Parent $PrivatePath)
        Assert-True (-not (Test-Path -LiteralPath $LegacyCache)) 'obsolete source-install cache remains'
        Assert-True (-not (Test-Path -LiteralPath $PrivatePath)) 'obsolete private helper remains'

        $DeveloperCache = Join-Path $LegacyCache 'rusty-v8-development'
        [void](New-Item -ItemType Directory -Force -Path $DeveloperCache)
        Remove-LegacyInstallState (Join-Path $TestRoot 'bin')
        Assert-True (Test-Path -LiteralPath $DeveloperCache -PathType Container) 'standalone developer V8 cache was claimed as installer state'
    }
    finally {
        $env:LOCALAPPDATA = $OriginalLocalAppData
    }

    foreach ($Forbidden in @(
            'cargo build',
            'rustup-init',
            'Visual Studio Build Tools',
            'GitHubArchiveRoot',
            'rg.exe',
            'Invoke-WebRequest'
        )) {
        Assert-True (-not $InstallerText.Contains($Forbidden)) "obsolete installer path remains: $Forbidden"
    }
    Assert-True $InstallerText.Contains('bcodex-x86_64-pc-windows-msvc.exe.gz') 'Windows release asset is missing'
    Assert-True $InstallerText.Contains('[IO.File]::Replace') 'atomic Windows replacement is missing'
    Assert-True $InstallerText.Contains('BCODEX_UPDATE_PARENT_PID') 'running-binary finalization is missing'
    Assert-True $InstallerText.Contains('ResponseHeadersRead') 'bounded streaming download is missing'

    $HelpOutput = & powershell.exe `
        -NoLogo `
        -NoProfile `
        -ExecutionPolicy Bypass `
        -File $InstallerPath `
        -Help 2>&1
    if ($LASTEXITCODE -ne 0) { throw 'install.ps1 -Help failed' }
    Assert-True (($HelpOutput -join "`n").Contains('No compilation is performed')) 'help omits prebuilt install behavior'

    Write-Host 'Windows prebuilt installer tests passed'
}
finally {
    Remove-Item -LiteralPath $TestRoot -Recurse -Force -ErrorAction SilentlyContinue
}
