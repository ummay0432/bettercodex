#Requires -Version 5.1

<#
.SYNOPSIS
Runs Cargo with the verified sandbox-enabled rusty_v8 artifacts used by bettercodex.
#>

[CmdletBinding()]
param(
    [Parameter(ValueFromRemainingArguments = $true)]
    [string[]] $CargoArguments
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'
[Net.ServicePointManager]::SecurityProtocol = [Net.SecurityProtocolType]::Tls12

$V8Version = '150.4.0'
$V8Profile = 'ptrcomp_sandbox_release'
$V8ReleaseUrl = "https://github.com/openai/codex/releases/download/rusty-v8-v$V8Version"
$Target = 'x86_64-pc-windows-msvc'
$ArchiveSha256 = '732ec5da4243aa166799780c8519a5eea6f32f6e47657a323342794dc3c239d6'
$BindingSha256 = 'dabf78ba1faac127660db9862b1d0354175c71b8db2d4fcb5bacbd9c93576b16'

function Fail([string] $Message) {
    throw "bettercodex Cargo setup: $Message"
}

function Get-FileSha256([string] $Path) {
    return (Get-FileHash -LiteralPath $Path -Algorithm SHA256).Hash.ToLowerInvariant()
}

function Get-VerifiedDownload(
    [string] $Url,
    [string] $Destination,
    [string] $ExpectedSha256
) {
    if ((Test-Path -LiteralPath $Destination -PathType Leaf) -and
        (Get-FileSha256 $Destination) -eq $ExpectedSha256) {
        return
    }

    $Directory = Split-Path -Parent $Destination
    [void](New-Item -ItemType Directory -Force -Path $Directory)
    $Temporary = "$Destination.tmp.$([Guid]::NewGuid().ToString('N'))"
    try {
        Write-Host "Downloading $(Split-Path -Leaf $Destination)"
        Invoke-WebRequest -UseBasicParsing -Uri $Url -OutFile $Temporary -MaximumRedirection 5
        $ActualSha256 = Get-FileSha256 $Temporary
        if ($ActualSha256 -ne $ExpectedSha256) {
            Fail "$(Split-Path -Leaf $Destination) has SHA-256 $ActualSha256, expected $ExpectedSha256"
        }
        Move-Item -LiteralPath $Temporary -Destination $Destination -Force
    }
    finally {
        Remove-Item -LiteralPath $Temporary -Force -ErrorAction SilentlyContinue
    }
}

$ScriptDirectory = Split-Path -Parent $MyInvocation.MyCommand.Path
$RepositoryRoot = Split-Path -Parent $ScriptDirectory
$Lockfile = Join-Path $RepositoryRoot 'Cargo.lock'
$LockText = [IO.File]::ReadAllText($Lockfile)
$V8Match = [regex]::Match(
    $LockText,
    '(?ms)^name = "v8"\r?\nversion = "([^"]+)"'
)
if (-not $V8Match.Success -or $V8Match.Groups[1].Value -ne $V8Version) {
    $Resolved = if ($V8Match.Success) { $V8Match.Groups[1].Value } else { 'unknown' }
    Fail "Cargo.lock resolves V8 $Resolved, but the verified artifact pair is $V8Version"
}

$CargoCommand = if ($env:CARGO) { $env:CARGO } else { 'cargo.exe' }
if ($env:V8_FROM_SOURCE -match '^(?i:1|true|yes)$') {
    & $CargoCommand @CargoArguments
    exit $LASTEXITCODE
}

$ArchiveOverride = $env:RUSTY_V8_ARCHIVE
$BindingOverride = $env:RUSTY_V8_SRC_BINDING_PATH
if ($ArchiveOverride -or $BindingOverride) {
    if (-not $ArchiveOverride -or -not $BindingOverride) {
        Fail 'RUSTY_V8_ARCHIVE and RUSTY_V8_SRC_BINDING_PATH must be set together'
    }
    & $CargoCommand @CargoArguments
    exit $LASTEXITCODE
}

$HostLine = (& rustc.exe -vV | Where-Object { $_ -like 'host: *' } | Select-Object -First 1)
if (-not $HostLine) {
    Fail 'rustc did not report its host target'
}
$HostTarget = $HostLine.Substring(6).Trim()
if ($HostTarget -ne $Target) {
    Fail "no verified sandboxed V8 artifact is available for Rust host $HostTarget"
}

$CacheRoot = if ($env:BCODEX_CACHE_DIR) {
    if (-not [IO.Path]::IsPathRooted($env:BCODEX_CACHE_DIR)) {
        Fail 'BCODEX_CACHE_DIR must be an absolute path'
    }
    $env:BCODEX_CACHE_DIR
}
elseif ($env:LOCALAPPDATA) {
    Join-Path $env:LOCALAPPDATA 'bettercodex\cache'
}
else {
    Join-Path ([IO.Path]::GetTempPath()) 'bettercodex-cache'
}

$ArtifactDirectory = Join-Path $CacheRoot "rusty-v8-$V8Version-$Target"
$ArchiveName = "rusty_v8_${V8Profile}_${Target}.lib.gz"
$BindingName = "src_binding_${V8Profile}_${Target}.rs"
$ArchivePath = Join-Path $ArtifactDirectory $ArchiveName
$BindingPath = Join-Path $ArtifactDirectory $BindingName

Get-VerifiedDownload "$V8ReleaseUrl/$ArchiveName" $ArchivePath $ArchiveSha256
Get-VerifiedDownload "$V8ReleaseUrl/$BindingName" $BindingPath $BindingSha256

$env:RUSTY_V8_ARCHIVE = $ArchivePath
$env:RUSTY_V8_SRC_BINDING_PATH = $BindingPath
& $CargoCommand @CargoArguments
exit $LASTEXITCODE
