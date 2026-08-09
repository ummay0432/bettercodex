#Requires -Version 5.1

<#
.SYNOPSIS
Installs one exact public bettercodex main revision from source on Windows.
#>

[CmdletBinding()]
param(
    [switch] $Help,
    [switch] $ValidateOnly
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'
[Net.ServicePointManager]::SecurityProtocol = [Net.SecurityProtocolType]::Tls12

$DefaultRepository = 'ummay0432/bettercodex'
$GitHubApiRoot = 'https://api.github.com'
$GitHubArchiveRoot = 'https://codeload.github.com'
$MaximumMetadataBytes = 1MB
$MaximumSourceArchiveBytes = 128MB
$MinimumWindowsBuild = 17763
$FirstBuildCacheHeadroom = 8GB
$WarmBuildCacheHeadroom = 2GB
$ScratchHeadroom = 2GB
$SourceHeadroom = 512MB
$InstallHeadroom = 256MB

function Write-Step([string] $Message) {
    Write-Host "==> $Message"
}

function Fail([string] $Message) {
    throw "bettercodex installer: $Message"
}

function Get-ProcessEnvironmentSnapshot {
    $Snapshot = @{}
    foreach ($Entry in [Environment]::GetEnvironmentVariables('Process').GetEnumerator()) {
        $Snapshot[[string]$Entry.Key] = [string]$Entry.Value
    }
    return $Snapshot
}

function Restore-ProcessEnvironment([hashtable] $Snapshot) {
    $CurrentEnvironment = [Environment]::GetEnvironmentVariables('Process')
    foreach ($Entry in $CurrentEnvironment.GetEnumerator()) {
        $Name = [string]$Entry.Key
        if (-not $Snapshot.ContainsKey($Name)) {
            Remove-Item -LiteralPath "Env:$Name" -ErrorAction SilentlyContinue
        }
    }
    foreach ($Name in $Snapshot.Keys) {
        [Environment]::SetEnvironmentVariable($Name, $Snapshot[$Name], 'Process')
    }
}

function Test-SourceRevision([string] $Revision) {
    return $Revision -cmatch '^[0-9a-fA-F]{40}$'
}

function Assert-AbsolutePath([string] $Value, [string] $Label) {
    if ([string]::IsNullOrWhiteSpace($Value) -or -not [IO.Path]::IsPathRooted($Value)) {
        Fail "$Label must be an absolute path"
    }
    if ($Value.IndexOfAny([char[]]@("`r", "`n", [char]0)) -ge 0) {
        Fail "$Label contains an invalid character"
    }
}

function Test-IsReparsePoint([string] $Path) {
    if (-not (Test-Path -LiteralPath $Path)) {
        return $false
    }
    return ([IO.File]::GetAttributes($Path) -band [IO.FileAttributes]::ReparsePoint) -ne 0
}

function Assert-NoReparsePath([string] $Path) {
    $Current = [IO.Path]::GetFullPath($Path)
    $Existing = New-Object 'System.Collections.Generic.List[string]'
    while (-not [string]::IsNullOrEmpty($Current)) {
        if (Test-Path -LiteralPath $Current) {
            $Existing.Add($Current)
        }
        $Parent = Split-Path -Parent $Current
        if ($Parent -eq $Current) { break }
        $Current = $Parent
    }
    foreach ($Entry in $Existing) {
        if (Test-IsReparsePoint $Entry) {
            Fail "refusing installer-owned path through reparse point $Entry"
        }
    }
}

function Get-VolumeSpace([string] $Path) {
    $Probe = [IO.Path]::GetFullPath($Path)
    while (-not (Test-Path -LiteralPath $Probe)) {
        $Parent = Split-Path -Parent $Probe
        if ([string]::IsNullOrEmpty($Parent) -or $Parent -eq $Probe) {
            Fail "could not find an existing parent for disk-space check at $Path"
        }
        $Probe = $Parent
    }
    $Drive = (Get-Item -LiteralPath $Probe -Force).PSDrive
    if ($null -eq $Drive -or $null -eq $Drive.Free) {
        Fail "could not determine free disk space for $Path"
    }
    return [pscustomobject]@{
        Root = [string]$Drive.Root
        Free = [long]$Drive.Free
    }
}

function Assert-FreeSpaceBudget([object[]] $Budgets) {
    $Volumes = @{}
    foreach ($Budget in $Budgets) {
        $Volume = Get-VolumeSpace ([string]$Budget.Path)
        $Key = $Volume.Root.ToLowerInvariant()
        if (-not $Volumes.ContainsKey($Key)) {
            $Volumes[$Key] = [pscustomobject]@{
                Root = $Volume.Root
                Free = $Volume.Free
                Required = [long]0
            }
        }
        $Volumes[$Key].Required += [long]$Budget.Bytes
    }
    foreach ($Volume in $Volumes.Values) {
        Write-Step ("Disk preflight: {0:N1} GiB free on {1}; {2:N1} GiB estimated build headroom required" -f
            ($Volume.Free / 1GB), $Volume.Root, ($Volume.Required / 1GB))
        if ($Volume.Free -lt $Volume.Required) {
            Fail ("insufficient disk space on {0}: {1:N1} GiB free, {2:N1} GiB required for this source build" -f
                $Volume.Root, ($Volume.Free / 1GB), ($Volume.Required / 1GB))
        }
    }
}

function Remove-OwnedTree([string] $Path) {
    if (-not (Test-Path -LiteralPath $Path)) { return }
    if (Test-IsReparsePoint $Path) {
        Remove-Item -LiteralPath $Path -Force
        return
    }
    if (Test-Path -LiteralPath $Path -PathType Container) {
        foreach ($Child in Get-ChildItem -LiteralPath $Path -Force) {
            Remove-OwnedTree $Child.FullName
        }
        Remove-Item -LiteralPath $Path -Force
    }
    else {
        Remove-Item -LiteralPath $Path -Force
    }
}

function Invoke-BoundedDownloadAttempt(
    [string] $Uri,
    [string] $Destination,
    [long] $MaximumBytes
) {
    Add-Type -AssemblyName System.Net.Http
    $Partial = "$Destination.partial.$([Guid]::NewGuid().ToString('N'))"
    $Handler = $null
    $Client = $null
    $Response = $null
    $Input = $null
    $Output = $null
    try {
        $Handler = New-Object Net.Http.HttpClientHandler
        $Handler.AllowAutoRedirect = $true
        $Handler.MaxAutomaticRedirections = 5
        $Client = New-Object Net.Http.HttpClient($Handler)
        $Client.Timeout = [TimeSpan]::FromMinutes(2)
        $Client.DefaultRequestHeaders.UserAgent.ParseAdd('bettercodex')
        $Response = $Client.GetAsync(
            $Uri,
            [Net.Http.HttpCompletionOption]::ResponseHeadersRead
        ).GetAwaiter().GetResult()
        $Response.EnsureSuccessStatusCode() | Out-Null
        if ($Response.Content.Headers.ContentLength -gt $MaximumBytes) {
            Fail "download from $Uri exceeds the $MaximumBytes-byte limit"
        }
        $Input = $Response.Content.ReadAsStreamAsync().GetAwaiter().GetResult()
        $Output = New-Object IO.FileStream(
            $Partial,
            [IO.FileMode]::CreateNew,
            [IO.FileAccess]::Write,
            [IO.FileShare]::None,
            81920,
            [IO.FileOptions]::WriteThrough
        )
        $Buffer = New-Object byte[] 81920
        [long] $Total = 0
        while (($Read = $Input.Read($Buffer, 0, $Buffer.Length)) -gt 0) {
            $Total += $Read
            if ($Total -gt $MaximumBytes) {
                Fail "download from $Uri exceeds the $MaximumBytes-byte limit"
            }
            $Output.Write($Buffer, 0, $Read)
        }
        if ($Total -eq 0) { Fail "download from $Uri was empty" }
        $Output.Flush($true)
        $Output.Dispose()
        $Output = $null
        $Input.Dispose()
        $Input = $null
        $Response.Dispose()
        $Response = $null
        Move-Item -LiteralPath $Partial -Destination $Destination -Force
    }
    finally {
        if ($null -ne $Output) { $Output.Dispose() }
        if ($null -ne $Input) { $Input.Dispose() }
        if ($null -ne $Response) { $Response.Dispose() }
        if ($null -ne $Client) { $Client.Dispose() }
        if ($null -ne $Handler) { $Handler.Dispose() }
        Remove-Item -LiteralPath $Partial -Force -ErrorAction SilentlyContinue
    }
}

function Invoke-BoundedDownload(
    [string] $Uri,
    [string] $Destination,
    [long] $MaximumBytes
) {
    for ($Attempt = 1; $Attempt -le 3; $Attempt++) {
        try {
            Invoke-BoundedDownloadAttempt $Uri $Destination $MaximumBytes
            return
        }
        catch {
            if ($Attempt -eq 3) { throw }
            Write-Warning "download from $Uri failed; retrying ($($Attempt + 1)/3)"
            Start-Sleep -Seconds $Attempt
        }
    }
}

function Resolve-MainRevision([string] $Repository) {
    $Uri = "$GitHubApiRoot/repos/$Repository/git/ref/heads/main"
    $Temporary = Join-Path ([IO.Path]::GetTempPath()) (
        'bettercodex-main-' + [Guid]::NewGuid().ToString('N') + '.json'
    )
    try {
        Invoke-BoundedDownload $Uri $Temporary $MaximumMetadataBytes
        $Response = [IO.File]::ReadAllText($Temporary) | ConvertFrom-Json
        if ($Response.ref -cne 'refs/heads/main' -or
            $Response.object.type -cne 'commit' -or
            -not (Test-SourceRevision ([string]$Response.object.sha))) {
            Fail 'GitHub returned an invalid bettercodex main revision'
        }
        return ([string]$Response.object.sha).ToLowerInvariant()
    }
    finally {
        Remove-Item -LiteralPath $Temporary -Force -ErrorAction SilentlyContinue
    }
}

function Import-VisualStudioEnvironment {
    if ((Get-Command cl.exe -ErrorAction SilentlyContinue) -and
        (Get-Command link.exe -ErrorAction SilentlyContinue)) {
        return
    }
    $InstallerRoot = [Environment]::GetFolderPath('ProgramFilesX86')
    $VsWhere = Join-Path $InstallerRoot 'Microsoft Visual Studio\Installer\vswhere.exe'
    if (-not (Test-Path -LiteralPath $VsWhere -PathType Leaf)) {
        Fail 'Microsoft Visual Studio 2022 C++ Build Tools and the Windows SDK are required (https://visualstudio.microsoft.com/visual-cpp-build-tools/)'
    }
    $Installation = (& $VsWhere -latest -products '*' -requires Microsoft.VisualStudio.Component.VC.Tools.x86.x64 -property installationPath | Select-Object -First 1)
    if ([string]::IsNullOrWhiteSpace($Installation)) {
        Fail 'Visual Studio 2022 is missing the Desktop development with C++ workload and Windows SDK'
    }
    $VsDevCmd = Join-Path $Installation 'Common7\Tools\VsDevCmd.bat'
    if (-not (Test-Path -LiteralPath $VsDevCmd -PathType Leaf)) {
        Fail "Visual Studio developer environment is unavailable at $VsDevCmd"
    }
    $EnvironmentLines = & cmd.exe /d /s /c "`"$VsDevCmd`" -no_logo -arch=x64 -host_arch=x64 >nul && set"
    if ($LASTEXITCODE -ne 0) {
        Fail 'Visual Studio C++ build environment could not be initialized'
    }
    foreach ($Line in $EnvironmentLines) {
        if ($Line -match '^([^=][^=]*)=(.*)$') {
            [Environment]::SetEnvironmentVariable($Matches[1], $Matches[2], 'Process')
        }
    }
    if (-not (Get-Command cl.exe -ErrorAction SilentlyContinue) -or
        -not (Get-Command link.exe -ErrorAction SilentlyContinue)) {
        Fail 'Visual Studio initialized without a usable x64 C++ compiler and linker'
    }
}

function Find-RustupExecutable(
    [string] $OriginalCargoHome,
    [string] $ManagedCargoHome
) {
    $Command = Get-Command rustup.exe -ErrorAction SilentlyContinue | Select-Object -First 1
    if ($null -ne $Command) { return [string]$Command.Source }

    $Candidates = New-Object 'System.Collections.Generic.List[string]'
    if (-not [string]::IsNullOrWhiteSpace($OriginalCargoHome) -and
        [IO.Path]::IsPathRooted($OriginalCargoHome)) {
        $Candidates.Add((Join-Path $OriginalCargoHome 'bin\rustup.exe'))
    }
    if ($env:USERPROFILE) {
        $Candidates.Add((Join-Path $env:USERPROFILE '.cargo\bin\rustup.exe'))
    }
    $Candidates.Add((Join-Path $ManagedCargoHome 'bin\rustup.exe'))
    foreach ($Candidate in $Candidates) {
        if (Test-Path -LiteralPath $Candidate -PathType Leaf) { return $Candidate }
    }
    return $null
}

function Find-PinnedRustTools([string] $Rustup, [string] $Toolchain) {
    $Cargo = (& $Rustup which --toolchain $Toolchain cargo 2>$null | Select-Object -First 1)
    if ($LASTEXITCODE -ne 0 -or [string]::IsNullOrWhiteSpace([string]$Cargo) -or
        -not (Test-Path -LiteralPath $Cargo -PathType Leaf)) {
        return $null
    }
    $Rustc = (& $Rustup which --toolchain $Toolchain rustc 2>$null | Select-Object -First 1)
    if ($LASTEXITCODE -ne 0 -or [string]::IsNullOrWhiteSpace([string]$Rustc) -or
        -not (Test-Path -LiteralPath $Rustc -PathType Leaf)) {
        return $null
    }
    return [pscustomobject]@{ Cargo = [string]$Cargo; Rustc = [string]$Rustc }
}

function Get-SourceInputHash([string] $SourceRoot) {
    $Inputs = New-Object 'System.Collections.Generic.List[IO.FileInfo]'
    foreach ($Relative in @('Cargo.toml', 'Cargo.lock', 'rust-toolchain.toml', 'scripts\cargo-with-v8.ps1', 'src')) {
        $InputPath = Join-Path $SourceRoot $Relative
        if (-not (Test-Path -LiteralPath $InputPath)) {
            Fail "source commit is missing release input $Relative"
        }
        if (Test-Path -LiteralPath $InputPath -PathType Container) {
            foreach ($File in Get-ChildItem -LiteralPath $InputPath -File -Recurse) {
                $Inputs.Add($File)
            }
        }
        else {
            $Inputs.Add((Get-Item -LiteralPath $InputPath))
        }
    }
    foreach ($Relative in @('.cargo', 'build.rs', 'bundled-skills', 'docs\evals', 'prompts')) {
        $InputPath = Join-Path $SourceRoot $Relative
        if (Test-Path -LiteralPath $InputPath -PathType Container) {
            foreach ($File in Get-ChildItem -LiteralPath $InputPath -File -Recurse) {
                $Inputs.Add($File)
            }
        }
        elseif (Test-Path -LiteralPath $InputPath -PathType Leaf) {
            $Inputs.Add((Get-Item -LiteralPath $InputPath))
        }
    }
    $Lines = foreach ($File in $Inputs) {
        if (($File.Attributes -band [IO.FileAttributes]::ReparsePoint) -ne 0) {
            Fail "source archive contains a reparse point at $($File.FullName)"
        }
        $Relative = $File.FullName.Substring($SourceRoot.Length).TrimStart('\').Replace('\', '/')
        "$(Get-FileHash -LiteralPath $File.FullName -Algorithm SHA256 | Select-Object -ExpandProperty Hash)  $Relative`n"
    }
    $Bytes = [Text.Encoding]::UTF8.GetBytes(($Lines | Sort-Object -CaseSensitive) -join '')
    $Hasher = [Security.Cryptography.SHA256]::Create()
    try {
        return ([BitConverter]::ToString($Hasher.ComputeHash($Bytes))).Replace('-', '').ToLowerInvariant()
    }
    finally {
        $Hasher.Dispose()
    }
}

function Prepend-PathEntry([string] $PathValue, [string] $Entry) {
    $Entries = New-Object 'System.Collections.Generic.List[string]'
    $Entries.Add($Entry.TrimEnd('\'))
    if (-not [string]::IsNullOrWhiteSpace($PathValue)) {
        foreach ($Segment in $PathValue.Split(';')) {
            $Trimmed = $Segment.Trim()
            if (-not [string]::IsNullOrWhiteSpace($Trimmed) -and
                -not $Trimmed.TrimEnd('\').Equals($Entry.TrimEnd('\'), [StringComparison]::OrdinalIgnoreCase)) {
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
            Write-Step 'PATH updated for future terminal sessions'
        }
    }
    catch {
        Write-Warning "could not update the per-user PATH; add $BinDirectory manually for future terminals: $_"
    }
    try {
        $env:Path = Prepend-PathEntry $env:Path $BinDirectory
    }
    catch {
        Write-Warning "could not update this process's PATH; run $BinDirectory\bcodex.exe directly: $_"
    }
}

function Invoke-WithRetry([scriptblock] $Operation, [string] $Description) {
    $Delay = 50
    for ($Attempt = 1; $Attempt -le 8; $Attempt++) {
        try {
            & $Operation
            return
        }
        catch [IO.IOException] {
            if ($Attempt -eq 8) { throw }
            Start-Sleep -Milliseconds $Delay
            $Delay = [Math]::Min($Delay * 2, 1000)
        }
        catch [UnauthorizedAccessException] {
            if ($Attempt -eq 8) { throw }
            Start-Sleep -Milliseconds $Delay
            $Delay = [Math]::Min($Delay * 2, 1000)
        }
    }
    Fail "could not $Description"
}

function Try-StageReusableBinary(
    [string[]] $Binaries,
    [string] $Candidate,
    [string] $Revision,
    [string] $ExpectedVersion,
    [string] $BuildInputHash
) {
    foreach ($ReusableBinary in $Binaries) {
        if (-not (Test-Path -LiteralPath $ReusableBinary -PathType Leaf)) { continue }

        $ReusableVersion = $null
        $VersionExitCode = -1
        try {
            $ReusableVersion = (& $ReusableBinary --version 2>$null) -join "`n"
            $VersionExitCode = $LASTEXITCODE
        }
        catch {
            # A damaged cached executable must not block a valid installed
            # executable or the source-build fallback.
        }
        if ($VersionExitCode -eq 0 -and $ReusableVersion.Trim() -ceq "bcodex $ExpectedVersion") {
            $StageExitCode = -1
            try {
                $null = & $ReusableBinary --internal-install-stage $Candidate $Revision $BuildInputHash 2>$null
                $StageExitCode = $LASTEXITCODE
            }
            catch {
                # The exact release-input hash is proved by the helper. Any
                # launch or staging failure falls through to the next source.
            }
            if ($StageExitCode -eq 0 -and
                (Test-Path -LiteralPath $Candidate -PathType Leaf) -and
                -not (Test-IsReparsePoint $Candidate)) {
                Write-Step 'Reusing a verified bettercodex executable for unchanged release inputs'
                return $true
            }
        }

        if (Test-Path -LiteralPath $Candidate) {
            Invoke-WithRetry {
                Remove-OwnedTree $Candidate
            } 'remove an unusable reusable-binary stage'
        }
    }
    return $false
}

function Recover-FinalizerArtifacts([string] $BinDirectory, [string] $Destination) {
    foreach ($Directory in Get-ChildItem -LiteralPath $BinDirectory -Directory -Force -Filter '.bcodex-finalize.*' -ErrorAction SilentlyContinue) {
        if ($Directory.Name -cnotmatch '^\.bcodex-finalize\.([0-9a-f]{32})$' -or
            ($Directory.Attributes -band [IO.FileAttributes]::ReparsePoint) -ne 0) {
            Fail "unsafe bettercodex finalizer artifact $($Directory.FullName)"
        }
        $TransactionId = $Matches[1]
        $ManifestPath = Join-Path $Directory.FullName 'transaction.json'
        if (-not (Test-Path -LiteralPath $ManifestPath)) {
            $Artifacts = @(Get-ChildItem -LiteralPath $Directory.FullName -Force)
            if ($Artifacts.Count -eq 0) {
                Invoke-WithRetry {
                    Remove-Item -LiteralPath $Directory.FullName -Force
                } 'remove the empty completed finalizer transaction'
                continue
            }
            Fail "incomplete bettercodex finalizer record $ManifestPath"
        }
        if (-not (Test-Path -LiteralPath $ManifestPath -PathType Leaf) -or
            (Test-IsReparsePoint $ManifestPath)) {
            Fail "incomplete bettercodex finalizer record $ManifestPath"
        }
        $Manifest = [IO.File]::ReadAllText($ManifestPath) | ConvertFrom-Json
        $ExpectedCandidate = Join-Path $Directory.FullName 'candidate.exe'
        $ExpectedBackup = Join-Path $Directory.FullName 'backup.exe'
        if ($Manifest.transaction_id -cne $TransactionId -or
            -not [string]::Equals([string]$Manifest.destination, $Destination, [StringComparison]::OrdinalIgnoreCase) -or
            -not [string]::Equals([string]$Manifest.candidate, $ExpectedCandidate, [StringComparison]::OrdinalIgnoreCase) -or
            -not [string]::Equals([string]$Manifest.backup, $ExpectedBackup, [StringComparison]::OrdinalIgnoreCase)) {
            Fail "invalid bettercodex finalizer record $ManifestPath"
        }
        foreach ($Artifact in @($ExpectedCandidate, $ExpectedBackup)) {
            if ((Test-Path -LiteralPath $Artifact) -and (Test-IsReparsePoint $Artifact)) {
                Fail "unsafe bettercodex finalizer artifact $Artifact"
            }
        }
        if (Test-Path -LiteralPath $ExpectedBackup -PathType Leaf) {
            # A retained backup means the previous command was moved but the
            # transaction did not finish its cleanup. Prefer the known previous
            # command even if the candidate reached the destination before a
            # crash; the requested update can then retry normally.
            $Interrupted = Join-Path $Directory.FullName 'interrupted.exe'
            if (Test-Path -LiteralPath $Interrupted) {
                Fail "unsafe bettercodex recovery artifact $Interrupted"
            }
            if (Test-Path -LiteralPath $Destination) {
                Invoke-WithRetry {
                    Move-Item -LiteralPath $Destination -Destination $Interrupted
                } 'preserve the interrupted update candidate'
            }
            try {
                Invoke-WithRetry {
                    Move-Item -LiteralPath $ExpectedBackup -Destination $Destination
                } 'restore the previous installed binary'
            }
            catch {
                if (-not (Test-Path -LiteralPath $Destination) -and
                    (Test-Path -LiteralPath $Interrupted -PathType Leaf)) {
                    Invoke-WithRetry {
                        Move-Item -LiteralPath $Interrupted -Destination $Destination
                    } 'restore the interrupted destination after recovery failure'
                }
                throw
            }
        }
        Remove-OwnedTree $Directory.FullName
    }
}

function Start-DeferredReplacement(
    [string] $ManifestPath,
    [int] $ParentPid,
    [long] $ParentStartTicks,
    [string] $PowerShellPath
) {
    $Finalizer = @'
Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'
function Retry([scriptblock]$Operation) {
    $delay = 50
    for ($attempt = 1; $attempt -le 10; $attempt++) {
        try { & $Operation; return } catch [IO.IOException] { if ($attempt -eq 10) { throw } } catch [UnauthorizedAccessException] { if ($attempt -eq 10) { throw } }
        Start-Sleep -Milliseconds $delay
        $delay = [Math]::Min($delay * 2, 1000)
    }
}
$manifestPath = $env:BCODEX_FINALIZE_MANIFEST
$parentPid = [int]$env:BCODEX_FINALIZE_PARENT_PID
$parentTicks = [long]$env:BCODEX_FINALIZE_PARENT_TICKS
$manifest = [IO.File]::ReadAllText($manifestPath) | ConvertFrom-Json
$root = Split-Path -Parent $manifestPath
if (-not [string]::Equals($root, (Split-Path -Parent $manifest.candidate), [StringComparison]::OrdinalIgnoreCase) -or -not [string]::Equals([string]$manifest.candidate, (Join-Path $root 'candidate.exe'), [StringComparison]::OrdinalIgnoreCase) -or -not [string]::Equals([string]$manifest.backup, (Join-Path $root 'backup.exe'), [StringComparison]::OrdinalIgnoreCase)) { throw 'invalid bettercodex finalizer paths' }
$lock = $null
$completed = $false
try {
    for ($attempt = 1; $attempt -le 300 -and $null -eq $lock; $attempt++) {
        try { $lock = New-Object IO.FileStream($manifest.lock, [IO.FileMode]::OpenOrCreate, [IO.FileAccess]::ReadWrite, [IO.FileShare]::None) } catch [IO.IOException] { Start-Sleep -Milliseconds 100 }
    }
    if ($null -eq $lock) { throw 'could not acquire the bettercodex install lock for finalization' }
    $parent = Get-Process -Id $parentPid -ErrorAction SilentlyContinue
    if ($null -ne $parent -and $parent.StartTime.ToUniversalTime().Ticks -eq $parentTicks -and -not $parent.WaitForExit(300000)) { throw 'timed out waiting for the bettercodex updater process to exit' }
    if ((Get-FileHash -LiteralPath $manifest.candidate -Algorithm SHA256).Hash.ToLowerInvariant() -cne $manifest.sha256) { throw 'staged bettercodex digest changed before finalization' }
    if (Test-Path -LiteralPath $manifest.backup) { Remove-Item -LiteralPath $manifest.backup -Force }
    if (Test-Path -LiteralPath $manifest.destination) { Retry { Move-Item -LiteralPath $manifest.destination -Destination $manifest.backup } }
    try {
        Retry { Move-Item -LiteralPath $manifest.candidate -Destination $manifest.destination }
        $version = (& $manifest.destination --version 2>$null) -join "`n"
        $revision = (& $manifest.destination --internal-source-revision 2>$null) -join "`n"
        if ($version.Trim() -cne "bcodex $($manifest.version)" -or $revision.Trim() -cne $manifest.revision) { throw 'updated bettercodex command failed final verification' }
        if (Test-Path -LiteralPath $manifest.backup) { Remove-Item -LiteralPath $manifest.backup -Force }
        $completed = $true
        Write-Host "==> Updated bcodex $($manifest.version) ($($manifest.revision.Substring(0, 12))) at $($manifest.destination)"
    } catch {
        Remove-Item -LiteralPath $manifest.destination -Force -ErrorAction SilentlyContinue
        if (Test-Path -LiteralPath $manifest.backup) { Retry { Move-Item -LiteralPath $manifest.backup -Destination $manifest.destination } }
        throw
    }
} finally {
    if ($null -ne $lock) { $lock.Dispose() }
    if ($completed) {
        Remove-Item -LiteralPath $manifestPath -Force -ErrorAction SilentlyContinue
        Remove-Item -LiteralPath $root -Force -ErrorAction SilentlyContinue
    }
}
'@
    $Bytes = [Text.Encoding]::Unicode.GetBytes($Finalizer)
    $Encoded = [Convert]::ToBase64String($Bytes)
    $FinalizeEnvironment = Get-ProcessEnvironmentSnapshot
    try {
        $env:BCODEX_FINALIZE_MANIFEST = $ManifestPath
        $env:BCODEX_FINALIZE_PARENT_PID = [string]$ParentPid
        $env:BCODEX_FINALIZE_PARENT_TICKS = [string]$ParentStartTicks
        $Started = Start-Process -FilePath $PowerShellPath -ArgumentList @(
            '-NoLogo', '-NoProfile', '-NonInteractive', '-ExecutionPolicy', 'Bypass',
            '-EncodedCommand', $Encoded
        ) -PassThru
    }
    finally {
        Restore-ProcessEnvironment $FinalizeEnvironment
    }
    if ($null -eq $Started -or $Started.HasExited) {
        Fail 'could not start the bettercodex update finalizer'
    }
    return $Started
}

if ($Help) {
    @'
Usage: install.ps1

Resolves the exact source revision at public main, builds and verifies it, and
installs bcodex.exe. Set BCODEX_INSTALL_DIR to an absolute custom binary
directory or BCODEX_INSTALL_REVISION to a full immutable commit ID.
'@ | Write-Host
    exit 0
}

if ($env:OS -cne 'Windows_NT') { Fail 'install.ps1 supports native Windows only' }
if (-not [Environment]::Is64BitOperatingSystem) { Fail '64-bit Windows is required' }
$Architecture = [Runtime.InteropServices.RuntimeInformation]::OSArchitecture.ToString()
if ($Architecture -cne 'X64') { Fail "unsupported Windows architecture: $Architecture" }
$WindowsBuild = [Environment]::OSVersion.Version.Build
if ($WindowsBuild -lt $MinimumWindowsBuild) {
    Fail "Windows build $MinimumWindowsBuild or newer is required"
}

$Repository = if ($env:BCODEX_REPOSITORY) { $env:BCODEX_REPOSITORY } else { $DefaultRepository }
if ($Repository -cnotmatch '^[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+$') {
    Fail 'BCODEX_REPOSITORY must be an owner/repository name'
}
if (-not $env:LOCALAPPDATA) { Fail 'LOCALAPPDATA is required' }
$BinDirectory = if ($env:BCODEX_INSTALL_DIR) {
    $env:BCODEX_INSTALL_DIR
} else {
    Join-Path $env:LOCALAPPDATA 'Programs\bettercodex\bin'
}
$CacheRoot = if ($env:BCODEX_CACHE_DIR) {
    $env:BCODEX_CACHE_DIR
} else {
    Join-Path $env:LOCALAPPDATA 'bettercodex\cache'
}
Assert-AbsolutePath $BinDirectory 'BCODEX_INSTALL_DIR'
Assert-AbsolutePath $CacheRoot 'BCODEX_CACHE_DIR'
$BinDirectory = [IO.Path]::GetFullPath($BinDirectory)
$CacheRoot = [IO.Path]::GetFullPath($CacheRoot)
Assert-NoReparsePath $BinDirectory
Assert-NoReparsePath $CacheRoot

if ($ValidateOnly) {
    Write-Step 'Windows installer validation passed'
    exit 0
}

[void](New-Item -ItemType Directory -Force -Path $BinDirectory)
[void](New-Item -ItemType Directory -Force -Path $CacheRoot)
$Destination = Join-Path $BinDirectory 'bcodex.exe'
if ((Test-Path -LiteralPath $Destination) -and
    (-not (Test-Path -LiteralPath $Destination -PathType Leaf) -or
        (Test-IsReparsePoint $Destination))) {
    Fail "refusing to replace unsafe bettercodex executable $Destination"
}

$LockPath = Join-Path $BinDirectory '.bcodex-install.lock'
if (Test-IsReparsePoint $LockPath) { Fail "refusing reparse-point installer lock $LockPath" }
$Lock = $null
$TemporaryRoot = $null
$FinalizeRoot = $null
$Deferred = $false
$PreserveFinalizeRoot = $false
try {
    try {
        $Lock = New-Object IO.FileStream(
            $LockPath,
            [IO.FileMode]::OpenOrCreate,
            [IO.FileAccess]::ReadWrite,
            [IO.FileShare]::None,
            4096,
            [IO.FileOptions]::WriteThrough
        )
    }
    catch [IO.IOException] {
        Fail 'another bettercodex install or update is already running'
    }
    $Transaction = "pid=$PID`nid=$([Guid]::NewGuid().ToString('N'))`n"
    $TransactionBytes = [Text.Encoding]::UTF8.GetBytes($Transaction)
    $Lock.SetLength(0)
    $Lock.Write($TransactionBytes, 0, $TransactionBytes.Length)
    $Lock.Flush($true)

    Recover-FinalizerArtifacts $BinDirectory $Destination

    $RequestedRevision = $env:BCODEX_INSTALL_REVISION
    if ($RequestedRevision -and -not (Test-SourceRevision $RequestedRevision)) {
        Fail 'BCODEX_INSTALL_REVISION must be a full 40-character commit ID'
    }
    $Revision = if ($RequestedRevision) {
        $RequestedRevision.ToLowerInvariant()
    } else {
        Resolve-MainRevision $Repository
    }

    if (Test-Path -LiteralPath $Destination -PathType Leaf) {
        $InstalledRevision = (& $Destination --internal-source-revision 2>$null) -join "`n"
        if ($LASTEXITCODE -eq 0 -and $InstalledRevision.Trim().Equals($Revision, [StringComparison]::OrdinalIgnoreCase)) {
            Write-Step "bettercodex is already current with main at $($Revision.Substring(0, 12))."
            Ensure-CommandPath $BinDirectory
            exit 0
        }
    }

    $TargetDirectory = Join-Path $CacheRoot 'build\x86_64-pc-windows-msvc\target'
    Assert-NoReparsePath $TargetDirectory
    Assert-FreeSpaceBudget @(
        [pscustomobject]@{ Path = [IO.Path]::GetTempPath(); Bytes = $SourceHeadroom },
        [pscustomobject]@{ Path = $BinDirectory; Bytes = $InstallHeadroom }
    )

    if (-not (Get-Command tar.exe -ErrorAction SilentlyContinue)) {
        Fail 'Windows tar.exe is required to extract the immutable source archive'
    }

    $TemporaryRoot = Join-Path ([IO.Path]::GetTempPath()) (
        'bettercodex-install.' + [Guid]::NewGuid().ToString('N')
    )
    [void](New-Item -ItemType Directory -Path $TemporaryRoot)
    $ArchivePath = Join-Path $TemporaryRoot 'source.tar.gz'
    $SourceRoot = Join-Path $TemporaryRoot 'source'
    $CompilerTemp = Join-Path $TemporaryRoot 'compiler-temp'
    $SmokeRoot = Join-Path $TemporaryRoot 'smoke'
    [void](New-Item -ItemType Directory -Path $SourceRoot)
    [void](New-Item -ItemType Directory -Path $CompilerTemp)

    Write-Step "Installing bettercodex main $($Revision.Substring(0, 12)) for Windows x64"
    Write-Step 'Downloading the immutable source snapshot'
    Invoke-BoundedDownload "$GitHubArchiveRoot/$Repository/tar.gz/$Revision" $ArchivePath $MaximumSourceArchiveBytes
    & tar.exe -xzf $ArchivePath -C $SourceRoot --strip-components=1
    if ($LASTEXITCODE -ne 0) { Fail 'downloaded source archive could not be extracted' }
    foreach ($Required in @('Cargo.toml', 'Cargo.lock', 'rust-toolchain.toml', 'scripts\cargo-with-v8.ps1')) {
        if (-not (Test-Path -LiteralPath (Join-Path $SourceRoot $Required) -PathType Leaf)) {
            Fail "source commit has no $Required"
        }
    }
    foreach ($Item in Get-ChildItem -LiteralPath $SourceRoot -Force -Recurse) {
        if (($Item.Attributes -band [IO.FileAttributes]::ReparsePoint) -ne 0) {
            Fail "source archive contains a reparse point at $($Item.FullName)"
        }
    }

    $CargoManifest = [IO.File]::ReadAllText((Join-Path $SourceRoot 'Cargo.toml'))
    $VersionMatch = [regex]::Match($CargoManifest, '(?m)^version = "([^"]+)"')
    if (-not $VersionMatch.Success) { Fail 'source commit has no package version' }
    $ExpectedVersion = $VersionMatch.Groups[1].Value
    $ToolchainFile = [IO.File]::ReadAllText((Join-Path $SourceRoot 'rust-toolchain.toml'))
    $ToolchainMatch = [regex]::Match($ToolchainFile, '(?m)^channel = "([A-Za-z0-9._-]+)"')
    if (-not $ToolchainMatch.Success) { Fail 'source commit has no pinned Rust toolchain' }
    $RustToolchain = $ToolchainMatch.Groups[1].Value

    $BuildInputHash = Get-SourceInputHash $SourceRoot
    $BuiltBinary = Join-Path $TargetDirectory 'release\bcodex.exe'
    $PowerShellPath = (Get-Process -Id $PID).Path
    $TransactionId = [Guid]::NewGuid().ToString('N')
    $FinalizeRoot = Join-Path $BinDirectory ".bcodex-finalize.$TransactionId"
    [void](New-Item -ItemType Directory -Path $FinalizeRoot)
    $Candidate = Join-Path $FinalizeRoot 'candidate.exe'
    $ReusableBinaries = @($BuiltBinary, $Destination)
    $ReusedExistingBinary = Try-StageReusableBinary `
        $ReusableBinaries $Candidate $Revision $ExpectedVersion $BuildInputHash

    if (-not $ReusedExistingBinary) {
        $HasWarmBuildCache = Test-Path -LiteralPath $BuiltBinary -PathType Leaf
        $CacheHeadroom = if ($HasWarmBuildCache) { $WarmBuildCacheHeadroom } else { $FirstBuildCacheHeadroom }
        Assert-FreeSpaceBudget @(
            [pscustomobject]@{ Path = $CacheRoot; Bytes = $CacheHeadroom },
            [pscustomobject]@{ Path = [IO.Path]::GetTempPath(); Bytes = $ScratchHeadroom },
            [pscustomobject]@{ Path = $BinDirectory; Bytes = $InstallHeadroom }
        )
        $OriginalCargoHome = $env:CARGO_HOME
        $CompilerEnvironment = Get-ProcessEnvironmentSnapshot
        try {
            Import-VisualStudioEnvironment
            $CargoHome = Join-Path $CacheRoot 'cargo'
            $ManagedRustupHome = Join-Path $CacheRoot 'rustup'
            [void](New-Item -ItemType Directory -Force -Path $CargoHome)
            [void](New-Item -ItemType Directory -Force -Path $TargetDirectory)
            $Rustup = Find-RustupExecutable $OriginalCargoHome $CargoHome
            $CachedRustup = Join-Path $CargoHome 'bin\rustup.exe'
            if (-not $Rustup) {
                if (-not (Test-Path -LiteralPath $CachedRustup -PathType Leaf)) {
                    Write-Step 'Installing rustup for the pinned bettercodex toolchain'
                    $RustupInit = Join-Path $TemporaryRoot 'rustup-init.exe'
                    Invoke-BoundedDownload 'https://win.rustup.rs/x86_64' $RustupInit 32MB
                    $env:CARGO_HOME = $CargoHome
                    $env:RUSTUP_HOME = $ManagedRustupHome
                    & $RustupInit -y --no-modify-path --profile minimal --default-toolchain none
                    if ($LASTEXITCODE -ne 0) { Fail 'the official rustup installer failed' }
                }
                $Rustup = $CachedRustup
            }
            $env:CARGO_HOME = $CargoHome
            Write-Step "Using the pinned Rust $RustToolchain toolchain"
            $UsesManagedRustupHome = [string]::Equals(
                [IO.Path]::GetFullPath($Rustup),
                [IO.Path]::GetFullPath($CachedRustup),
                [StringComparison]::OrdinalIgnoreCase
            )
            if ($UsesManagedRustupHome) { $env:RUSTUP_HOME = $ManagedRustupHome }
            $RustTools = Find-PinnedRustTools $Rustup $RustToolchain
            if ($null -eq $RustTools) {
                # Keep an existing user rustup installation untouched when it does not
                # already provide the pinned release. Missing toolchains belong to the
                # reusable bettercodex cache, matching the Unix installer.
                $env:RUSTUP_HOME = $ManagedRustupHome
                [void](New-Item -ItemType Directory -Force -Path $ManagedRustupHome)
                $RustTools = Find-PinnedRustTools $Rustup $RustToolchain
                if ($null -eq $RustTools) {
                    Write-Step "Caching missing Rust $RustToolchain at $ManagedRustupHome"
                    & $Rustup toolchain install $RustToolchain --profile minimal
                    if ($LASTEXITCODE -ne 0) { Fail "could not install pinned Rust $RustToolchain" }
                    $RustTools = Find-PinnedRustTools $Rustup $RustToolchain
                }
            }
            if ($null -eq $RustTools) {
                Fail 'pinned Cargo and rustc executables are unavailable'
            }
            $Cargo = $RustTools.Cargo
            $Rustc = $RustTools.Rustc

            Write-Step "Compiling bettercodex $ExpectedVersion with the warm Cargo cache"
            $BuildEnvironmentNames = @(
                'BCODEX_BUILD_INPUT_HASH',
                'BCODEX_CACHE_DIR',
                'BCODEX_SOURCE_REVISION',
                'CARGO',
                'CARGO_BUILD_BUILD_DIR',
                'CARGO_BUILD_JOBS',
                'CARGO_BUILD_TARGET',
                'CARGO_BUILD_TARGET_DIR',
                'CARGO_ENCODED_RUSTFLAGS',
                'CARGO_INCREMENTAL',
                'CARGO_INSTALL_ROOT',
                'CARGO_TARGET_DIR',
                'RUSTC',
                'RUSTC_WORKSPACE_WRAPPER',
                'RUSTC_WRAPPER',
                'RUSTFLAGS',
                'RUSTUP_TOOLCHAIN',
                'RUSTY_V8_ARCHIVE',
                'RUSTY_V8_SRC_BINDING_PATH',
                'TEMP',
                'TMP',
                'V8_FROM_SOURCE'
            )
            $BuildEnvironment = Get-ProcessEnvironmentSnapshot
            foreach ($Name in $BuildEnvironmentNames) {
                Remove-Item -LiteralPath "Env:$Name" -ErrorAction SilentlyContinue
            }
            $OriginalPath = $env:Path
            try {
                $env:BCODEX_BUILD_INPUT_HASH = $BuildInputHash
                $env:BCODEX_CACHE_DIR = $CacheRoot
                $env:CARGO = $Cargo
                $env:CARGO_INCREMENTAL = '1'
                $env:CARGO_TARGET_DIR = $TargetDirectory
                $env:RUSTC = $Rustc
                $env:TEMP = $CompilerTemp
                $env:TMP = $CompilerTemp
                $env:Path = (Split-Path -Parent $Cargo) + ';' + $OriginalPath
                Push-Location $SourceRoot
                try {
                    & $PowerShellPath -NoLogo -NoProfile -NonInteractive -ExecutionPolicy Bypass -File (Join-Path $SourceRoot 'scripts\cargo-with-v8.ps1') build --release --locked --bin bcodex
                    if ($LASTEXITCODE -ne 0) { Fail 'local bettercodex compilation failed' }
                }
                finally {
                    Pop-Location
                }
            }
            finally {
                Restore-ProcessEnvironment $BuildEnvironment
            }

            if (-not (Test-Path -LiteralPath $BuiltBinary -PathType Leaf)) {
                Fail 'local build did not produce bcodex.exe'
            }
            $BuiltVersion = (& $BuiltBinary --version 2>$null) -join "`n"
            if ($LASTEXITCODE -ne 0 -or $BuiltVersion.Trim() -cne "bcodex $ExpectedVersion") {
                Fail "built binary did not report bcodex $ExpectedVersion"
            }
            & $BuiltBinary --internal-install-stage $Candidate $Revision $BuildInputHash
            if ($LASTEXITCODE -ne 0) { Fail "built binary could not stage source revision $Revision" }
        }
        finally {
            Restore-ProcessEnvironment $CompilerEnvironment
        }
    }
    $CandidateVersion = (& $Candidate --version 2>$null) -join "`n"
    $CandidateRevision = (& $Candidate --internal-source-revision 2>$null) -join "`n"
    if ($CandidateVersion.Trim() -cne "bcodex $ExpectedVersion" -or $CandidateRevision.Trim() -cne $Revision) {
        Fail 'staged binary could not be verified'
    }

    Write-Step 'Smoke-testing V8 and every embedded system resource'
    foreach ($Directory in @('profile', 'local-app-data', 'codex-home', 'bcodex-home', 'workspace')) {
        [void](New-Item -ItemType Directory -Force -Path (Join-Path $SmokeRoot $Directory))
    }
    $SmokeEnvironment = Get-ProcessEnvironmentSnapshot
    $PriorDirectory = [Environment]::CurrentDirectory
    try {
        $env:USERPROFILE = Join-Path $SmokeRoot 'profile'
        $env:LOCALAPPDATA = Join-Path $SmokeRoot 'local-app-data'
        $env:CODEX_HOME = Join-Path $SmokeRoot 'codex-home'
        $env:BCODEX_HOME = Join-Path $SmokeRoot 'bcodex-home'
        $env:BCODEX_SKIP_UPDATE_CHECK = '1'
        [Environment]::CurrentDirectory = Join-Path $SmokeRoot 'workspace'
        $SmokeOutput = (& $Candidate --internal-install-smoke 2>$null) -join "`n"
        $SmokeExitCode = $LASTEXITCODE
    }
    finally {
        [Environment]::CurrentDirectory = $PriorDirectory
        Restore-ProcessEnvironment $SmokeEnvironment
    }
    if ($SmokeExitCode -ne 0 -or $SmokeOutput.Trim() -cne "bcodex $ExpectedVersion install smoke passed") {
        Fail 'staged binary failed its runtime and embedded-resource smoke test'
    }

    $CandidateSha256 = (Get-FileHash -LiteralPath $Candidate -Algorithm SHA256).Hash.ToLowerInvariant()
    $Backup = Join-Path $FinalizeRoot 'backup.exe'
    $ManifestPath = Join-Path $FinalizeRoot 'transaction.json'
    $ParentPid = 0
    $ParentStartTicks = 0
    if ($env:BCODEX_UPDATE_PARENT_PID -match '^[1-9][0-9]*$') {
        $ParentPid = [int]$env:BCODEX_UPDATE_PARENT_PID
        $Parent = Get-Process -Id $ParentPid -ErrorAction Stop
        $ParentStartTicks = $Parent.StartTime.ToUniversalTime().Ticks
    }
    $Manifest = [ordered]@{
        transaction_id = $TransactionId
        destination = $Destination
        candidate = $Candidate
        backup = $Backup
        lock = $LockPath
        sha256 = $CandidateSha256
        revision = $Revision
        version = $ExpectedVersion
    }
    [IO.File]::WriteAllText($ManifestPath, ($Manifest | ConvertTo-Json -Compress), (New-Object Text.UTF8Encoding($false)))

    if ($ParentPid -gt 0) {
        [void](Start-DeferredReplacement $ManifestPath $ParentPid $ParentStartTicks $PowerShellPath)
        $Deferred = $true
        Write-Step 'Verified update staged; replacement will finish after this bcodex process exits'
    }
    else {
        if (Test-Path -LiteralPath $Destination) {
            Invoke-WithRetry { Move-Item -LiteralPath $Destination -Destination $Backup } 'stage the installed binary backup'
        }
        try {
            Invoke-WithRetry { Move-Item -LiteralPath $Candidate -Destination $Destination } 'install bcodex.exe'
            $InstalledVersion = (& $Destination --version 2>$null) -join "`n"
            $InstalledRevision = (& $Destination --internal-source-revision 2>$null) -join "`n"
            if ($InstalledVersion.Trim() -cne "bcodex $ExpectedVersion" -or $InstalledRevision.Trim() -cne $Revision) {
                Fail 'installed binary could not be verified'
            }
            Remove-Item -LiteralPath $Backup -Force -ErrorAction SilentlyContinue
        }
        catch {
            $InstallFailure = $_
            try {
                if (Test-Path -LiteralPath $Destination) {
                    Invoke-WithRetry {
                        Remove-Item -LiteralPath $Destination -Force
                    } 'remove the failed bettercodex candidate'
                }
                if (Test-Path -LiteralPath $Backup) {
                    Invoke-WithRetry {
                        Move-Item -LiteralPath $Backup -Destination $Destination
                    } 'restore the previous installed binary'
                }
            }
            catch {
                $PreserveFinalizeRoot = $true
                throw "bettercodex installation failed and rollback needs recovery from $ManifestPath`: $_"
            }
            throw $InstallFailure
        }
        Remove-OwnedTree $FinalizeRoot
        $FinalizeRoot = $null
        Write-Step "Installed bcodex $ExpectedVersion ($($Revision.Substring(0, 12))) at $Destination"
    }

    Ensure-CommandPath $BinDirectory
    if (-not $Deferred) { Write-Step 'Run: bcodex login' }
}
finally {
    if ($null -ne $Lock) { $Lock.Dispose() }
    if ($TemporaryRoot) {
        try { Remove-OwnedTree $TemporaryRoot } catch { Write-Warning "could not remove task-owned installer tree $TemporaryRoot`: $_" }
    }
    if ($FinalizeRoot -and -not $Deferred -and -not $PreserveFinalizeRoot) {
        try { Remove-OwnedTree $FinalizeRoot } catch { Write-Warning "could not remove failed finalizer tree $FinalizeRoot`: $_" }
    }
}
