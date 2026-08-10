#Requires -Version 5.1

[CmdletBinding()]
param()

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

$RepositoryRoot = Split-Path -Parent (Split-Path -Parent $MyInvocation.MyCommand.Path)
$InstallerPath = Join-Path $RepositoryRoot 'scripts\install.ps1'
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
$global:MaximumBinaryBytes = 128MB
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

function Assert-Eventually([scriptblock] $Condition, [string] $Message) {
    $Deadline = [DateTime]::UtcNow.AddSeconds(15)
    while ([DateTime]::UtcNow -lt $Deadline) {
        if (& $Condition) { return }
        Start-Sleep -Milliseconds 100
    }
    throw $Message
}

function Write-Candidate(
    [string] $Path,
    [string] $ReleaseTag,
    [string] $Version
) {
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
    Assert-True (-not (Test-ReleaseTag "bcodex-v1.2.3-$('A' * 40)")) 'uppercase revision was accepted'
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

    $Destination = Join-Path $TestRoot 'installed.cmd'
    $Backup = Join-Path $TestRoot 'backup.cmd'
    Write-Candidate $Destination "bcodex-v1.2.2-$('2' * 40)" '1.2.2'
    $CandidateDigest = Get-FileSha256 $Candidate
    Install-Candidate $Candidate $Destination $CandidateDigest $Backup
    Assert-True (Test-BinaryIdentity $Destination $Tag '1.2.3') 'candidate was not installed'
    Assert-True (-not (Test-Path -LiteralPath $Backup)) 'successful install retained backup'

    $OldDigest = Get-FileSha256 $Destination
    $InvalidCandidate = Join-Path $TestRoot 'invalid-candidate.cmd'
    Write-Candidate $InvalidCandidate "bcodex-v9.9.9-$('9' * 40)" '9.9.9'
    $RollbackFailed = $false
    try {
        Install-Candidate $InvalidCandidate $Destination $OldDigest $Backup
    }
    catch {
        $RollbackFailed = $true
    }
    Assert-True $RollbackFailed 'invalid replacement unexpectedly succeeded'
    Assert-Equal (Get-FileSha256 $Destination) $OldDigest 'failed replacement did not roll back'
    Assert-True (-not (Test-Path -LiteralPath $Backup)) 'rollback retained its transaction backup'

    $FreshCandidate = Join-Path $TestRoot 'fresh-invalid-candidate.cmd'
    $FreshDestination = Join-Path $TestRoot 'fresh-installed.cmd'
    Write-Candidate $FreshCandidate "bcodex-v9.9.9-$('9' * 40)" '9.9.9'
    $FreshInstallFailed = $false
    try {
        Install-Candidate $FreshCandidate $FreshDestination $OldDigest $Backup
    }
    catch {
        $FreshInstallFailed = $true
    }
    Assert-True $FreshInstallFailed 'invalid fresh installation unexpectedly succeeded'
    Assert-True (-not (Test-Path -LiteralPath $FreshDestination)) 'failed fresh installation retained its destination'

    $PowerShellPath = (Get-Process -Id $PID).Path
    $DeferredDestination = Join-Path $TestRoot 'deferred-installed.cmd'
    $DeferredCandidate = Join-Path $TestRoot 'deferred-candidate.cmd'
    $DeferredLock = Join-Path $TestRoot 'deferred-install.lock'
    Write-Candidate $DeferredDestination "bcodex-v1.2.2-$('2' * 40)" '1.2.2'
    Write-Candidate $DeferredCandidate $Tag '1.2.3'
    $Waiter = Start-Process -FilePath $PowerShellPath -ArgumentList @(
        '-NoLogo', '-NoProfile', '-NonInteractive', '-Command',
        'Start-Sleep -Milliseconds 750'
    ) -WindowStyle Hidden -PassThru
    Start-DeferredReplacement `
        $DeferredCandidate `
        $DeferredDestination `
        '1.2.3' `
        (Get-FileSha256 $DeferredCandidate) `
        $Waiter.Id `
        $Waiter.StartTime.ToUniversalTime().Ticks `
        $DeferredLock
    Assert-Eventually {
        (-not (Test-Path -LiteralPath $DeferredCandidate)) -and
        (-not (Test-Path -LiteralPath $DeferredLock))
    } 'deferred replacement did not finish'
    Assert-True (Test-BinaryIdentity $DeferredDestination $Tag '1.2.3') 'deferred replacement did not install its candidate'
    Assert-Equal @(
        Get-ChildItem -LiteralPath $TestRoot -Filter 'deferred-installed.cmd.backup.*'
    ).Count 0 'deferred replacement retained a transaction backup'

    $DeferredDigest = Get-FileSha256 $DeferredDestination
    $DeferredInvalidCandidate = Join-Path $TestRoot 'deferred-invalid-candidate.cmd'
    Write-Candidate $DeferredInvalidCandidate "bcodex-v9.9.9-$('9' * 40)" '9.9.9'
    $Waiter = Start-Process -FilePath $PowerShellPath -ArgumentList @(
        '-NoLogo', '-NoProfile', '-NonInteractive', '-Command',
        'Start-Sleep -Milliseconds 750'
    ) -WindowStyle Hidden -PassThru
    Start-DeferredReplacement `
        $DeferredInvalidCandidate `
        $DeferredDestination `
        '1.2.3' `
        $DeferredDigest `
        $Waiter.Id `
        $Waiter.StartTime.ToUniversalTime().Ticks `
        $DeferredLock
    Assert-Eventually {
        (-not (Test-Path -LiteralPath $DeferredInvalidCandidate)) -and
        (-not (Test-Path -LiteralPath $DeferredLock))
    } 'rejected deferred replacement did not finish cleanup'
    Assert-Equal (Get-FileSha256 $DeferredDestination) $DeferredDigest 'rejected deferred replacement changed the installation'
    Assert-Equal @(
        Get-ChildItem -LiteralPath $TestRoot -Filter 'deferred-installed.cmd.backup.*'
    ).Count 0 'failed deferred replacement retained a transaction backup'

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
