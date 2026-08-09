#Requires -Version 5.1

<#
.SYNOPSIS
Installs the latest published prebuilt bettercodex binary on Windows.
#>

[CmdletBinding()]
param(
    [switch] $Help
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'
[Net.ServicePointManager]::SecurityProtocol = [Net.SecurityProtocolType]::Tls12

$DefaultRepository = 'ummay0432/bettercodex'
$AssetName = 'bcodex-x86_64-pc-windows-msvc.exe.gz'
$MaximumArchiveBytes = 128MB
$MaximumBinaryBytes = 256MB
$MinimumWindowsBuild = 22000

function Write-Step([string] $Message) {
    Write-Host "==> $Message"
}

function Fail([string] $Message) {
    throw "bettercodex installer: $Message"
}

function Assert-AbsolutePath([string] $Path, [string] $Name) {
    if ([string]::IsNullOrWhiteSpace($Path) -or -not [IO.Path]::IsPathRooted($Path)) {
        Fail "$Name must be an absolute path"
    }
}

function Test-IsReparsePoint([string] $Path) {
    if (-not (Test-Path -LiteralPath $Path)) { return $false }
    return ((Get-Item -LiteralPath $Path -Force).Attributes -band
        [IO.FileAttributes]::ReparsePoint) -ne 0
}

function Test-ReleaseTag([string] $Tag) {
    return $Tag -cmatch '^bcodex-v([0-9]+\.[0-9]+\.[0-9]+)-[0-9a-fA-F]{40}$'
}

function Get-ReleaseVersion([string] $Tag) {
    if ($Tag -cnotmatch '^bcodex-v([0-9]+\.[0-9]+\.[0-9]+)-[0-9a-fA-F]{40}$') {
        Fail "invalid bettercodex release tag $Tag"
    }
    return $Matches[1]
}

function Get-FileSha256([string] $Path) {
    return (Get-FileHash -LiteralPath $Path -Algorithm SHA256).Hash.ToLowerInvariant()
}

function Invoke-Download([string] $Uri, [string] $Destination) {
    Add-Type -AssemblyName System.Net.Http
    for ($Attempt = 1; $Attempt -le 3; $Attempt++) {
        Remove-Item -LiteralPath $Destination -Force -ErrorAction SilentlyContinue
        $Handler = $null
        $Client = $null
        $Response = $null
        $DownloadStream = $null
        $DestinationStream = $null
        $Failure = $null
        try {
            $Handler = New-Object Net.Http.HttpClientHandler
            $Handler.AllowAutoRedirect = $true
            $Handler.MaxAutomaticRedirections = 5
            $Client = New-Object Net.Http.HttpClient($Handler)
            $Client.Timeout = [TimeSpan]::FromMinutes(5)
            $Client.DefaultRequestHeaders.UserAgent.ParseAdd('bettercodex-installer')
            $Response = $Client.GetAsync(
                $Uri,
                [Net.Http.HttpCompletionOption]::ResponseHeadersRead
            ).GetAwaiter().GetResult()
            $Response.EnsureSuccessStatusCode() | Out-Null
            $ContentLength = $Response.Content.Headers.ContentLength
            if ($null -ne $ContentLength -and $ContentLength -gt $MaximumArchiveBytes) {
                Fail 'downloaded release asset exceeds the allowed size'
            }
            $DownloadStream = $Response.Content.ReadAsStreamAsync().GetAwaiter().GetResult()
            $DestinationStream = New-Object IO.FileStream(
                $Destination,
                [IO.FileMode]::CreateNew,
                [IO.FileAccess]::Write,
                [IO.FileShare]::None,
                81920,
                [IO.FileOptions]::WriteThrough
            )
            $Buffer = New-Object byte[] 81920
            [long] $Total = 0
            while (($Read = $DownloadStream.Read($Buffer, 0, $Buffer.Length)) -gt 0) {
                $Total += $Read
                if ($Total -gt $MaximumArchiveBytes) {
                    Fail 'downloaded release asset exceeds the allowed size'
                }
                $DestinationStream.Write($Buffer, 0, $Read)
            }
            if ($Total -eq 0) { Fail 'downloaded release asset is empty' }
            $DestinationStream.Flush($true)
        }
        catch {
            $Failure = $_
        }
        finally {
            if ($null -ne $DestinationStream) { $DestinationStream.Dispose() }
            if ($null -ne $DownloadStream) { $DownloadStream.Dispose() }
            if ($null -ne $Response) { $Response.Dispose() }
            if ($null -ne $Client) { $Client.Dispose() }
            if ($null -ne $Handler) { $Handler.Dispose() }
        }
        if ($null -eq $Failure) { return }
        Remove-Item -LiteralPath $Destination -Force -ErrorAction SilentlyContinue
        if ($Attempt -eq 3) { throw $Failure }
        Write-Warning "download failed; retrying ($($Attempt + 1)/3)"
        Start-Sleep -Seconds $Attempt
    }
}

function Remove-EmptyDirectory([string] $Path) {
    if (-not (Test-Path -LiteralPath $Path -PathType Container)) { return }
    if ($null -eq (Get-ChildItem -LiteralPath $Path -Force -ErrorAction SilentlyContinue |
            Select-Object -First 1)) {
        Remove-Item -LiteralPath $Path -Force -ErrorAction SilentlyContinue
    }
}

function Remove-ObsoleteCacheDirectory([string] $Path) {
    if ((Test-Path -LiteralPath $Path -PathType Container) -and
        -not (Test-IsReparsePoint $Path)) {
        Remove-Item -LiteralPath $Path -Recurse -Force -ErrorAction SilentlyContinue
    }
}

function Remove-ObsoleteV8Caches([string] $CacheRoot) {
    foreach ($Directory in Get-ChildItem `
            -LiteralPath $CacheRoot `
            -Directory `
            -Force `
            -Filter 'rusty-v8-*' `
            -ErrorAction SilentlyContinue) {
        if (-not (Test-IsReparsePoint $Directory.FullName)) {
            Remove-Item -LiteralPath $Directory.FullName -Recurse -Force -ErrorAction SilentlyContinue
        }
    }
}

function Expand-GzipBinary([string] $Archive, [string] $Destination) {
    $ArchiveStream = $null
    $Gzip = $null
    $DestinationStream = $null
    try {
        $ArchiveStream = New-Object IO.FileStream(
            $Archive,
            [IO.FileMode]::Open,
            [IO.FileAccess]::Read,
            [IO.FileShare]::Read
        )
        $Gzip = New-Object IO.Compression.GZipStream(
            $ArchiveStream,
            [IO.Compression.CompressionMode]::Decompress,
            $false
        )
        $DestinationStream = New-Object IO.FileStream(
            $Destination,
            [IO.FileMode]::CreateNew,
            [IO.FileAccess]::Write,
            [IO.FileShare]::None,
            81920,
            [IO.FileOptions]::WriteThrough
        )
        $Buffer = New-Object byte[] 81920
        [long] $Total = 0
        while (($Read = $Gzip.Read($Buffer, 0, $Buffer.Length)) -gt 0) {
            $Total += $Read
            if ($Total -gt $MaximumBinaryBytes) {
                Fail 'decompressed bettercodex binary exceeds the allowed size'
            }
            $DestinationStream.Write($Buffer, 0, $Read)
        }
        if ($Total -eq 0) { Fail 'downloaded bettercodex binary is empty' }
        $DestinationStream.Flush($true)
    }
    finally {
        if ($null -ne $DestinationStream) { $DestinationStream.Dispose() }
        if ($null -ne $Gzip) { $Gzip.Dispose() }
        if ($null -ne $ArchiveStream) { $ArchiveStream.Dispose() }
    }
}

function Get-BinaryReleaseTag([string] $Binary) {
    try {
        $Output = (& $Binary --internal-release-tag 2>$null) -join "`n"
        if ($LASTEXITCODE -ne 0) { return $null }
        return $Output.Trim()
    }
    catch {
        return $null
    }
}

function Test-BinaryIdentity(
    [string] $Binary,
    [string] $ExpectedTag,
    [string] $ExpectedVersion
) {
    if ((Get-BinaryReleaseTag $Binary) -cne $ExpectedTag) { return $false }
    try {
        $Version = (& $Binary --version 2>$null) -join "`n"
        return $LASTEXITCODE -eq 0 -and $Version.Trim() -ceq "bcodex $ExpectedVersion"
    }
    catch {
        return $false
    }
}

function Invoke-BinarySmoke([string] $Binary, [string] $ExpectedVersion) {
    $SmokeRoot = Join-Path ([IO.Path]::GetTempPath()) (
        'bettercodex-smoke.' + [Guid]::NewGuid().ToString('N')
    )
    $VariableNames = @(
        'USERPROFILE', 'LOCALAPPDATA', 'CODEX_HOME', 'BCODEX_HOME',
        'BCODEX_SKIP_UPDATE_CHECK'
    )
    $Previous = @{}
    foreach ($Name in $VariableNames) {
        $Previous[$Name] = [Environment]::GetEnvironmentVariable($Name, 'Process')
    }
    try {
        foreach ($Name in @('profile', 'local-app-data', 'codex-home', 'bcodex-home')) {
            [void](New-Item -ItemType Directory -Force -Path (Join-Path $SmokeRoot $Name))
        }
        $env:USERPROFILE = Join-Path $SmokeRoot 'profile'
        $env:LOCALAPPDATA = Join-Path $SmokeRoot 'local-app-data'
        $env:CODEX_HOME = Join-Path $SmokeRoot 'codex-home'
        $env:BCODEX_HOME = Join-Path $SmokeRoot 'bcodex-home'
        $env:BCODEX_SKIP_UPDATE_CHECK = '1'
        $Output = (& $Binary --internal-install-smoke 2>$null) -join "`n"
        if ($LASTEXITCODE -ne 0 -or
            $Output.Trim() -cne "bcodex $ExpectedVersion install smoke passed") {
            Fail 'downloaded binary failed its runtime smoke test'
        }
    }
    finally {
        foreach ($Name in $VariableNames) {
            [Environment]::SetEnvironmentVariable($Name, $Previous[$Name], 'Process')
        }
        Remove-Item -LiteralPath $SmokeRoot -Recurse -Force -ErrorAction SilentlyContinue
    }
}

function Invoke-WithRetry([scriptblock] $Operation, [string] $Description) {
    $Delay = 50
    for ($Attempt = 1; $Attempt -le 10; $Attempt++) {
        try {
            & $Operation
            return
        }
        catch [IO.IOException] {
            if ($Attempt -eq 10) { throw }
        }
        catch [UnauthorizedAccessException] {
            if ($Attempt -eq 10) { throw }
        }
        Start-Sleep -Milliseconds $Delay
        $Delay = [Math]::Min($Delay * 2, 1000)
    }
    Fail "could not $Description"
}

function Restore-Backup(
    [string] $Backup,
    [string] $Destination,
    [string] $ExpectedSha256
) {
    Invoke-WithRetry {
        [IO.File]::Copy($Backup, $Destination, $true)
    } 'restore the previous bettercodex binary'
    if ((Get-FileSha256 $Destination) -cne $ExpectedSha256) {
        Fail 'restored bettercodex binary does not match the previous installation'
    }
    Invoke-WithRetry {
        [IO.File]::Delete($Backup)
    } 'remove the bettercodex rollback backup'
}

function Install-Candidate(
    [string] $Candidate,
    [string] $Destination,
    [string] $ExpectedTag,
    [string] $ExpectedVersion,
    [string] $Backup
) {
    $HadDestination = Test-Path -LiteralPath $Destination -PathType Leaf
    $PreviousSha256 = if ($HadDestination) { Get-FileSha256 $Destination } else { $null }
    try {
        if ($HadDestination) {
            Invoke-WithRetry {
                [IO.File]::Replace($Candidate, $Destination, $Backup, $true)
            } 'replace the installed bettercodex binary'
        }
        else {
            [IO.File]::Move($Candidate, $Destination)
        }
        if (-not (Test-BinaryIdentity $Destination $ExpectedTag $ExpectedVersion)) {
            Fail 'installed binary failed final verification'
        }
        if (Test-Path -LiteralPath $Backup -PathType Leaf) {
            Invoke-WithRetry {
                [IO.File]::Delete($Backup)
            } 'remove the bettercodex rollback backup'
        }
    }
    catch {
        $Failure = $_
        try {
            if (Test-Path -LiteralPath $Backup -PathType Leaf) {
                Restore-Backup $Backup $Destination $PreviousSha256
            }
            elseif (-not $HadDestination) {
                Invoke-WithRetry {
                    if (Test-Path -LiteralPath $Destination) {
                        [IO.File]::Delete($Destination)
                    }
                } 'remove the failed bettercodex installation'
            }
            elseif ((-not (Test-Path -LiteralPath $Destination -PathType Leaf)) -or
                (Get-FileSha256 $Destination) -cne $PreviousSha256) {
                Fail 'replacement failed without a recoverable bettercodex backup'
            }
        }
        catch {
            $Recovery = if (Test-Path -LiteralPath $Backup -PathType Leaf) {
                " Previous binary retained at $Backup."
            }
            else { '' }
            throw "installation failed: $($Failure.Exception.Message); rollback failed: $($_.Exception.Message).$Recovery"
        }
        throw $Failure
    }
}

function Start-DeferredReplacement(
    [string] $Candidate,
    [string] $Destination,
    [string] $ExpectedTag,
    [string] $ExpectedVersion,
    [string] $CandidateSha256,
    [int] $ParentPid,
    [long] $ParentStartTicks,
    [string] $LockPath
) {
    $Finalizer = @'
Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'
function Retry([scriptblock] $Operation) {
    $delay = 50
    for ($attempt = 1; $attempt -le 10; $attempt++) {
        try { & $Operation; return } catch [IO.IOException] { if ($attempt -eq 10) { throw } } catch [UnauthorizedAccessException] { if ($attempt -eq 10) { throw } }
        Start-Sleep -Milliseconds $delay
        $delay = [Math]::Min($delay * 2, 1000)
    }
}
function Restore([string] $Backup, [string] $Destination, [string] $ExpectedSha256) {
    Retry { [IO.File]::Copy($Backup, $Destination, $true) }
    if ((Get-FileHash -LiteralPath $Destination -Algorithm SHA256).Hash.ToLowerInvariant() -cne $ExpectedSha256) { throw 'restored bettercodex binary does not match the previous installation' }
    Retry { [IO.File]::Delete($Backup) }
}
$candidate = $env:BCODEX_FINALIZE_CANDIDATE
$destination = $env:BCODEX_FINALIZE_DESTINATION
$expectedTag = $env:BCODEX_FINALIZE_TAG
$expectedVersion = $env:BCODEX_FINALIZE_VERSION
$expectedSha256 = $env:BCODEX_FINALIZE_SHA256
$parentPid = [int]$env:BCODEX_FINALIZE_PARENT_PID
$parentTicks = [long]$env:BCODEX_FINALIZE_PARENT_TICKS
$lockPath = $env:BCODEX_FINALIZE_LOCK
$backup = "$destination.backup.$([Guid]::NewGuid().ToString('N'))"
$lock = $null
$hadDestination = $false
$previousSha256 = $null
try {
    $parent = Get-Process -Id $parentPid -ErrorAction SilentlyContinue
    if ($null -ne $parent -and $parent.StartTime.ToUniversalTime().Ticks -eq $parentTicks -and -not $parent.WaitForExit(300000)) { throw 'timed out waiting for the bettercodex updater process to exit' }
    for ($attempt = 1; $attempt -le 300 -and $null -eq $lock; $attempt++) {
        try { $lock = New-Object IO.FileStream($lockPath, [IO.FileMode]::OpenOrCreate, [IO.FileAccess]::ReadWrite, [IO.FileShare]::None) } catch [IO.IOException] { Start-Sleep -Milliseconds 100 }
    }
    if ($null -eq $lock) { throw 'could not acquire the bettercodex install lock' }
    if ((Get-FileHash -LiteralPath $candidate -Algorithm SHA256).Hash.ToLowerInvariant() -cne $expectedSha256) { throw 'staged bettercodex digest changed before finalization' }
    $hadDestination = Test-Path -LiteralPath $destination -PathType Leaf
    if ($hadDestination) {
        $previousSha256 = (Get-FileHash -LiteralPath $destination -Algorithm SHA256).Hash.ToLowerInvariant()
        Retry { [IO.File]::Replace($candidate, $destination, $backup, $true) }
    } else { [IO.File]::Move($candidate, $destination) }
    $tag = (& $destination --internal-release-tag 2>$null) -join "`n"
    $version = (& $destination --version 2>$null) -join "`n"
    if ($tag.Trim() -cne $expectedTag -or $version.Trim() -cne "bcodex $expectedVersion") { throw 'updated bettercodex command failed final verification' }
    if (Test-Path -LiteralPath $backup -PathType Leaf) { Retry { [IO.File]::Delete($backup) } }
    Write-Host "==> Updated bcodex $expectedVersion at $destination"
} catch {
    $failure = $_
    try {
        if (Test-Path -LiteralPath $backup -PathType Leaf) {
            Restore $backup $destination $previousSha256
        } elseif (-not $hadDestination) {
            Retry { if (Test-Path -LiteralPath $destination) { [IO.File]::Delete($destination) } }
        } elseif ((-not (Test-Path -LiteralPath $destination -PathType Leaf)) -or (Get-FileHash -LiteralPath $destination -Algorithm SHA256).Hash.ToLowerInvariant() -cne $previousSha256) {
            throw 'replacement failed without a recoverable bettercodex backup'
        }
    } catch {
        $recovery = if (Test-Path -LiteralPath $backup -PathType Leaf) { " Previous binary retained at $backup." } else { '' }
        throw "update failed: $($failure.Exception.Message); rollback failed: $($_.Exception.Message).$recovery"
    }
    throw $failure
} finally {
    if ($null -ne $lock) { $lock.Dispose() }
    Remove-Item -LiteralPath $candidate -Force -ErrorAction SilentlyContinue
    Remove-Item -LiteralPath $lockPath -Force -ErrorAction SilentlyContinue
}
'@
    $Encoded = [Convert]::ToBase64String([Text.Encoding]::Unicode.GetBytes($Finalizer))
    $Names = @(
        'BCODEX_FINALIZE_CANDIDATE', 'BCODEX_FINALIZE_DESTINATION',
        'BCODEX_FINALIZE_TAG', 'BCODEX_FINALIZE_VERSION', 'BCODEX_FINALIZE_SHA256',
        'BCODEX_FINALIZE_PARENT_PID', 'BCODEX_FINALIZE_PARENT_TICKS',
        'BCODEX_FINALIZE_LOCK'
    )
    $Previous = @{}
    foreach ($Name in $Names) {
        $Previous[$Name] = [Environment]::GetEnvironmentVariable($Name, 'Process')
    }
    try {
        $env:BCODEX_FINALIZE_CANDIDATE = $Candidate
        $env:BCODEX_FINALIZE_DESTINATION = $Destination
        $env:BCODEX_FINALIZE_TAG = $ExpectedTag
        $env:BCODEX_FINALIZE_VERSION = $ExpectedVersion
        $env:BCODEX_FINALIZE_SHA256 = $CandidateSha256
        $env:BCODEX_FINALIZE_PARENT_PID = [string]$ParentPid
        $env:BCODEX_FINALIZE_PARENT_TICKS = [string]$ParentStartTicks
        $env:BCODEX_FINALIZE_LOCK = $LockPath
        $PowerShellPath = (Get-Process -Id $PID).Path
        $Process = Start-Process -FilePath $PowerShellPath -ArgumentList @(
            '-NoLogo', '-NoProfile', '-NonInteractive', '-ExecutionPolicy', 'Bypass',
            '-EncodedCommand', $Encoded
        ) -WindowStyle Hidden -PassThru
    }
    finally {
        foreach ($Name in $Names) {
            [Environment]::SetEnvironmentVariable($Name, $Previous[$Name], 'Process')
        }
    }
    if ($null -eq $Process -or $Process.HasExited) {
        Fail 'could not start the bettercodex update finalizer'
    }
}

function Prepend-PathEntry([string] $PathValue, [string] $Entry) {
    $Entries = New-Object 'System.Collections.Generic.List[string]'
    $Entries.Add($Entry.TrimEnd('\'))
    if (-not [string]::IsNullOrWhiteSpace($PathValue)) {
        foreach ($Segment in $PathValue.Split(';')) {
            $Trimmed = $Segment.Trim()
            if (-not [string]::IsNullOrWhiteSpace($Trimmed) -and
                -not $Trimmed.TrimEnd('\').Equals(
                    $Entry.TrimEnd('\'),
                    [StringComparison]::OrdinalIgnoreCase
                )) {
                $Entries.Add($Trimmed)
            }
        }
    }
    return $Entries -join ';'
}

function Ensure-CommandPath([string] $BinDirectory) {
    try {
        $UserPath = [Environment]::GetEnvironmentVariable('Path', 'User')
        $NewUserPath = Prepend-PathEntry $UserPath $BinDirectory
        if ($NewUserPath -cne $UserPath) {
            [Environment]::SetEnvironmentVariable('Path', $NewUserPath, 'User')
            Write-Step 'PATH updated for future terminal sessions; open a new terminal'
        }
    }
    catch {
        Write-Warning "could not update the user PATH; add $BinDirectory manually"
    }
}

function Remove-LegacyInstallState([string] $BinDirectory) {
    $CacheRoot = Join-Path $env:LOCALAPPDATA 'bettercodex\cache'
    if ((Test-Path -LiteralPath $CacheRoot -PathType Container) -and
        -not (Test-IsReparsePoint $CacheRoot)) {
        $LegacySourceInstall = $false
        foreach ($Name in @('build', 'cargo', 'rustup', 'tmp', 'downloads')) {
            $Legacy = Join-Path $CacheRoot $Name
            if ((Test-Path -LiteralPath $Legacy -PathType Container) -and
                -not (Test-IsReparsePoint $Legacy)) {
                $LegacySourceInstall = $true
                Remove-ObsoleteCacheDirectory $Legacy
            }
        }
        if ($LegacySourceInstall) { Remove-ObsoleteV8Caches $CacheRoot }
        Remove-EmptyDirectory $CacheRoot
    }
    $PrivatePath = Join-Path $BinDirectory 'bcodex-path'
    Remove-ObsoleteCacheDirectory $PrivatePath
}

if ($Help) {
    @'
Usage: install.ps1

Downloads, verifies, and atomically installs the Windows x64 binary from the
latest published bettercodex GitHub release. No compilation is performed.

Environment:
  BCODEX_INSTALL_DIR          Binary directory override.
  BCODEX_REPOSITORY           GitHub owner/repository override.
  BCODEX_INSTALL_RELEASE_TAG  Exact release tag used internally by updates.
'@ | Write-Host
    exit 0
}

if ($env:OS -cne 'Windows_NT') { Fail 'install.ps1 supports native Windows only' }
if (-not [Environment]::Is64BitOperatingSystem) { Fail '64-bit Windows is required' }
$Architecture = [Runtime.InteropServices.RuntimeInformation]::OSArchitecture.ToString()
if ($Architecture -cne 'X64') { Fail "unsupported Windows architecture: $Architecture" }
$WindowsBuild = [Environment]::OSVersion.Version.Build
if ($WindowsBuild -lt $MinimumWindowsBuild) {
    Fail "Windows 11 build $MinimumWindowsBuild or newer is required"
}
if (-not $env:LOCALAPPDATA) { Fail 'LOCALAPPDATA is required' }

$Repository = if ($env:BCODEX_REPOSITORY) { $env:BCODEX_REPOSITORY } else { $DefaultRepository }
if ($Repository -cnotmatch '^[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+$') {
    Fail 'BCODEX_REPOSITORY must be an owner/repository name'
}
$ExpectedTag = if ($env:BCODEX_INSTALL_RELEASE_TAG) {
    $env:BCODEX_INSTALL_RELEASE_TAG
} else {
    ''
}
if ($ExpectedTag -and -not (Test-ReleaseTag $ExpectedTag)) {
    Fail 'BCODEX_INSTALL_RELEASE_TAG is invalid'
}

$BinDirectory = if ($env:BCODEX_INSTALL_DIR) {
    $env:BCODEX_INSTALL_DIR
} else {
    Join-Path $env:LOCALAPPDATA 'Programs\bettercodex\bin'
}
Assert-AbsolutePath $BinDirectory 'BCODEX_INSTALL_DIR'
$BinDirectory = [IO.Path]::GetFullPath($BinDirectory)
[void](New-Item -ItemType Directory -Force -Path $BinDirectory)
if (Test-IsReparsePoint $BinDirectory) { Fail "refusing reparse-point install directory $BinDirectory" }

$Destination = Join-Path $BinDirectory 'bcodex.exe'
if ((Test-Path -LiteralPath $Destination) -and
    ((-not (Test-Path -LiteralPath $Destination -PathType Leaf)) -or
        (Test-IsReparsePoint $Destination))) {
    Fail "refusing unsafe bettercodex destination $Destination"
}

$TagPath = if ($ExpectedTag) { "download/$ExpectedTag" } else { 'latest/download' }
$DownloadUrl = "https://github.com/$Repository/releases/$TagPath/$AssetName"
$TransactionId = [Guid]::NewGuid().ToString('N')
$Archive = Join-Path $BinDirectory ".bcodex-download.$TransactionId.gz"
$Candidate = Join-Path $BinDirectory ".bcodex-stage.$TransactionId.exe"
$Backup = Join-Path $BinDirectory ".bcodex-backup.$TransactionId.exe"
$LockPath = Join-Path $BinDirectory '.bcodex-install.lock'
$Lock = $null
$Deferred = $false

try {
    try {
        $Lock = New-Object IO.FileStream(
            $LockPath,
            [IO.FileMode]::OpenOrCreate,
            [IO.FileAccess]::ReadWrite,
            [IO.FileShare]::None
        )
    }
    catch [IO.IOException] {
        Fail 'another bettercodex installation is already running'
    }

    Write-Step 'Downloading bettercodex for Windows x86-64'
    Invoke-Download $DownloadUrl $Archive
    Expand-GzipBinary $Archive $Candidate
    Remove-Item -LiteralPath $Archive -Force

    $CandidateTag = Get-BinaryReleaseTag $Candidate
    if (-not $CandidateTag -or -not (Test-ReleaseTag $CandidateTag)) {
        Fail 'downloaded binary has no valid bettercodex release tag'
    }
    if ($ExpectedTag -and $CandidateTag -cne $ExpectedTag) {
        Fail "downloaded binary is $CandidateTag, expected $ExpectedTag"
    }
    $CandidateVersion = Get-ReleaseVersion $CandidateTag
    if (-not (Test-BinaryIdentity $Candidate $CandidateTag $CandidateVersion)) {
        Fail 'downloaded binary version does not match its release tag'
    }

    Write-Step "Verifying bettercodex $CandidateVersion"
    Invoke-BinarySmoke $Candidate $CandidateVersion
    $CandidateSha256 = Get-FileSha256 $Candidate

    if ($env:BCODEX_UPDATE_PARENT_PID -match '^[1-9][0-9]*$') {
        $ParentPid = [int]$env:BCODEX_UPDATE_PARENT_PID
        $Parent = Get-Process -Id $ParentPid -ErrorAction Stop
        $ParentStartTicks = $Parent.StartTime.ToUniversalTime().Ticks
        Start-DeferredReplacement `
            $Candidate `
            $Destination `
            $CandidateTag `
            $CandidateVersion `
            $CandidateSha256 `
            $ParentPid `
            $ParentStartTicks `
            $LockPath
        $Deferred = $true
        Write-Step 'Verified update staged; replacement will finish after this bcodex process exits'
    }
    else {
        Install-Candidate $Candidate $Destination $CandidateTag $CandidateVersion $Backup
        Write-Step "Installed bcodex $CandidateVersion at $Destination"
    }

    Remove-LegacyInstallState $BinDirectory
    Ensure-CommandPath $BinDirectory
    if (-not $Deferred) { Write-Step 'Run: bcodex login' }
}
finally {
    if ($null -ne $Lock) { $Lock.Dispose() }
    Remove-Item -LiteralPath $Archive -Force -ErrorAction SilentlyContinue
    if (-not $Deferred) {
        Remove-Item -LiteralPath $Candidate -Force -ErrorAction SilentlyContinue
    }
    if (-not $Deferred) {
        Remove-Item -LiteralPath $LockPath -Force -ErrorAction SilentlyContinue
    }
}
