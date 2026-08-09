#Requires -Version 5.1

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'

function Assert([bool] $Condition, [string] $Message) {
    if (-not $Condition) { throw "Windows installer test: $Message" }
}

function Invoke-InstallerFunctions([string] $InstallerPath) {
    $Tokens = $null
    $Errors = $null
    $Ast = [Management.Automation.Language.Parser]::ParseFile(
        $InstallerPath,
        [ref]$Tokens,
        [ref]$Errors
    )
    if ($Errors.Count -ne 0) { throw ($Errors -join "`n") }
    $Names = @(
        'Write-Step',
        'Fail',
        'Get-ProcessEnvironmentSnapshot',
        'Restore-ProcessEnvironment',
        'Test-SourceRevision',
        'Test-IsReparsePoint',
        'Assert-NoReparsePath',
        'Get-VolumeSpace',
        'Assert-FreeSpaceBudget',
        'Remove-OwnedTree',
        'Get-BundledRipgrepPackage',
        'Test-InstalledBundledRipgrep',
        'Install-BundledRipgrep',
        'Recover-BundledRipgrepArtifacts',
        'Prepend-PathEntry',
        'Invoke-WithRetry',
        'Try-StageReusableBinary',
        'Recover-FinalizerArtifacts',
        'Start-DeferredReplacement'
    )
    $Definitions = $Ast.FindAll({
        param($Node)
        $Node -is [Management.Automation.Language.FunctionDefinitionAst] -and
            $Names -contains $Node.Name
    }, $true)
    Assert ($Definitions.Count -eq $Names.Count) 'installer helper extraction is incomplete'
    foreach ($Definition in $Definitions) {
        $DefinitionText = $Definition.Extent.Text
        $NameOffset = $DefinitionText.IndexOf($Definition.Name, [StringComparison]::Ordinal)
        Assert ($NameOffset -ge 0) "could not locate installer helper $($Definition.Name)"
        $GlobalDefinition = $DefinitionText.Insert($NameOffset, 'global:')
        Invoke-Expression $GlobalDefinition
    }
}

function Build-Fixture(
    [string] $Rustc,
    [string] $Root,
    [string] $Name,
    [string] $Revision,
    [string] $BuildInputHash = ''
) {
    $SourcePath = Join-Path $Root "$Name.rs"
    $Destination = Join-Path $Root "$Name.exe"
    $Template = @'
use std::env;
use std::fs;

const REVISION: &str = "__REVISION__";
const BUILD_INPUT_HASH: &str = "__BUILD_INPUT_HASH__";

fn main() {
    match env::args().nth(1).as_deref() {
        Some("--version") => println!("bcodex 0.1.2"),
        Some("--internal-source-revision") => println!("{REVISION}"),
        Some("--internal-install-stage") => {
            let arguments = env::args().collect::<Vec<_>>();
            if arguments.len() != 5 {
                std::process::exit(3);
            }
            fs::copy(env::current_exe().unwrap(), &arguments[2]).unwrap();
            if arguments[3] != REVISION || arguments[4] != BUILD_INPUT_HASH {
                std::process::exit(4);
            }
        }
        _ => std::process::exit(2),
    }
}
'@
    [IO.File]::WriteAllText(
        $SourcePath,
        $Template.Replace('__REVISION__', $Revision).Replace('__BUILD_INPUT_HASH__', $BuildInputHash),
        (New-Object Text.UTF8Encoding($false))
    )
    & $Rustc --edition=2024 $SourcePath -o $Destination
    if ($LASTEXITCODE -ne 0 -or -not (Test-Path -LiteralPath $Destination -PathType Leaf)) {
        throw "Windows installer test: rustc could not build $Name"
    }
    return $Destination
}

function Build-RipgrepFixture([string] $Rustc, [string] $Root) {
    $SourcePath = Join-Path $Root 'ripgrep.rs'
    $Destination = Join-Path $Root 'rg.exe'
    [IO.File]::WriteAllText(
        $SourcePath,
        @'
use std::env;

fn main() {
    if env::args().nth(1).as_deref() == Some("--version") {
        println!("ripgrep 15.2.0");
    } else {
        std::process::exit(2);
    }
}
'@,
        (New-Object Text.UTF8Encoding($false))
    )
    & $Rustc --edition=2024 $SourcePath -o $Destination
    if ($LASTEXITCODE -ne 0 -or -not (Test-Path -LiteralPath $Destination -PathType Leaf)) {
        throw 'Windows installer test: rustc could not build the ripgrep fixture'
    }
    return $Destination
}

function New-Transaction(
    [string] $TestRoot,
    [string] $Destination,
    [string] $CandidateFixture,
    [string] $Revision,
    [string] $Sha256
) {
    $TransactionId = [Guid]::NewGuid().ToString('N')
    $TransactionRoot = Join-Path $TestRoot ".bcodex-finalize.$TransactionId"
    [void](New-Item -ItemType Directory -Path $TransactionRoot)
    $Candidate = Join-Path $TransactionRoot 'candidate.exe'
    $Backup = Join-Path $TransactionRoot 'backup.exe'
    $ManifestPath = Join-Path $TransactionRoot 'transaction.json'
    Copy-Item -LiteralPath $CandidateFixture -Destination $Candidate
    $Manifest = [ordered]@{
        transaction_id = $TransactionId
        destination = $Destination
        candidate = $Candidate
        backup = $Backup
        lock = (Join-Path $TestRoot '.bcodex-install.lock')
        sha256 = $Sha256
        version = '0.1.2'
        revision = $Revision
    }
    [IO.File]::WriteAllText(
        $ManifestPath,
        ($Manifest | ConvertTo-Json -Compress),
        (New-Object Text.UTF8Encoding($false))
    )
    return [pscustomobject]@{
        Root = $TransactionRoot
        Candidate = $Candidate
        Backup = $Backup
        Manifest = $ManifestPath
    }
}

function Start-TestParent([string] $PowerShellPath, [int] $Milliseconds) {
    return Start-Process -FilePath $PowerShellPath -ArgumentList @(
        '-NoLogo',
        '-NoProfile',
        '-NonInteractive',
        '-ExecutionPolicy',
        'Bypass',
        '-Command',
        "Start-Sleep -Milliseconds $Milliseconds"
    ) -PassThru
}

$ScriptDirectory = Split-Path -Parent $MyInvocation.MyCommand.Path
$InstallerPath = Join-Path $ScriptDirectory 'install.ps1'
Invoke-InstallerFunctions $InstallerPath

$RipgrepManifest = Join-Path $ScriptDirectory 'codex_package\rg'
$RipgrepPackage = Get-BundledRipgrepPackage $RipgrepManifest 'windows-x86_64'
Assert ($RipgrepPackage.Size -eq 1789611) 'upstream ripgrep archive size drifted unexpectedly'
Assert ($RipgrepPackage.Digest -ceq '71b2fef860abe467217a538ff31de02f5258807c0129f771846f87bd029aafc5') 'upstream ripgrep digest drifted unexpectedly'
Assert ($RipgrepPackage.Version -ceq '15.2.0') 'upstream ripgrep version drifted unexpectedly'
Assert ($RipgrepPackage.MemberPath -ceq 'ripgrep-15.2.0-x86_64-pc-windows-msvc/rg.exe') 'upstream ripgrep member drifted unexpectedly'

Assert (Test-SourceRevision 'ABCDEFabcdefABCDEFabcdefABCDEFabcdefABCD') 'valid revision was rejected'
Assert (-not (Test-SourceRevision '111111111111111111111111111111111111111g')) 'invalid revision was accepted'
$EnvironmentProbe = 'BCODEX_INSTALL_TEST_' + [Guid]::NewGuid().ToString('N')
$EnvironmentSnapshot = Get-ProcessEnvironmentSnapshot
[Environment]::SetEnvironmentVariable($EnvironmentProbe, 'temporary', 'Process')
Restore-ProcessEnvironment $EnvironmentSnapshot
Assert ($null -eq [Environment]::GetEnvironmentVariable($EnvironmentProbe, 'Process')) 'compiler environment cleanup leaked a variable'
$UpdatedPath = Prepend-PathEntry 'C:\Other;C:\TOOLS\bettercodex\bin\' 'c:\tools\bettercodex\bin'
Assert ($UpdatedPath -ceq 'c:\tools\bettercodex\bin;C:\Other') 'PATH update did not deduplicate and prepend'

Assert-FreeSpaceBudget @(
    [pscustomobject]@{ Path = [IO.Path]::GetTempPath(); Bytes = 1MB }
)
function Get-VolumeSpace([string] $Path) {
    return [pscustomobject]@{ Root = 'C:\'; Free = 5MB }
}
$DiskFailure = $false
try {
    Assert-FreeSpaceBudget @(
        [pscustomobject]@{ Path = 'C:\cache'; Bytes = 3MB },
        [pscustomobject]@{ Path = 'C:\scratch'; Bytes = 3MB }
    )
}
catch {
    $DiskFailure = $_.Exception.Message -like '*insufficient disk space*'
}
Assert $DiskFailure 'same-volume disk budgets were not aggregated'

$Rustc = (Get-Command rustc.exe -ErrorAction SilentlyContinue | Select-Object -ExpandProperty Source -First 1)
if (-not $Rustc) {
    $Rustc = (Get-Command rustc -ErrorAction Stop | Select-Object -ExpandProperty Source -First 1)
}
$TestRoot = Join-Path ([IO.Path]::GetTempPath()) (
    'bettercodex installer test ' + [char]0x00fc + ' ' + [Guid]::NewGuid().ToString('N')
)
$Processes = New-Object 'System.Collections.Generic.List[System.Diagnostics.Process]'
try {
    [void](New-Item -ItemType Directory -Path $TestRoot)
    $OldRevision = '1111111111111111111111111111111111111111'
    $NewRevision = '2222222222222222222222222222222222222222'
    $OldFixture = Build-Fixture $Rustc $TestRoot 'old' $OldRevision
    $NewFixture = Build-Fixture $Rustc $TestRoot 'new' $NewRevision
    $BuildInputHash = 'a' * 64
    $ReusableFixture = Build-Fixture $Rustc $TestRoot 'reusable' $NewRevision $BuildInputHash
    $RipgrepFixture = Build-RipgrepFixture $Rustc $TestRoot
    $Destination = Join-Path $TestRoot 'bcodex.exe'
    $PowerShellPath = (Get-Process -Id $PID).Path

    $RipgrepBin = Join-Path $TestRoot 'ripgrep-bin'
    [void](New-Item -ItemType Directory -Path $RipgrepBin)
    $RipgrepExecutableDigest = (Get-FileHash -LiteralPath $RipgrepFixture -Algorithm SHA256).Hash.ToLowerInvariant()
    $RipgrepBundle = [pscustomobject]@{
        Candidate = $RipgrepFixture
        ArchiveDigest = $RipgrepPackage.Digest
        ExecutableDigest = $RipgrepExecutableDigest
        SourceRevision = $NewRevision
        Version = $RipgrepPackage.Version
    }
    Install-BundledRipgrep $RipgrepBundle $RipgrepBin
    Assert (Test-InstalledBundledRipgrep $RipgrepBin $RipgrepPackage $NewRevision) 'private ripgrep installation did not verify'
    $MetadataOnlyRevision = '3333333333333333333333333333333333333333'
    $MetadataOnlyBundle = [pscustomobject]@{
        Candidate = $null
        ArchiveDigest = $RipgrepPackage.Digest
        ExecutableDigest = $RipgrepExecutableDigest
        SourceRevision = $MetadataOnlyRevision
        Version = $RipgrepPackage.Version
    }
    Install-BundledRipgrep $MetadataOnlyBundle $RipgrepBin
    Assert (Test-InstalledBundledRipgrep $RipgrepBin $RipgrepPackage $MetadataOnlyRevision) 'unchanged private ripgrep metadata was not advanced to the selected source'
    Assert ((Get-FileHash -LiteralPath (Join-Path $RipgrepBin 'bcodex-path\rg.exe') -Algorithm SHA256).Hash.ToLowerInvariant() -ceq $RipgrepExecutableDigest) 'metadata-only ripgrep update changed executable bytes'
    Install-BundledRipgrep $RipgrepBundle $RipgrepBin
    $InvalidRipgrepBundle = [pscustomobject]@{
        Candidate = $RipgrepFixture
        ArchiveDigest = ('b' * 64) -join ''
        ExecutableDigest = $RipgrepExecutableDigest
        SourceRevision = $NewRevision
        Version = '99.0.0'
    }
    $RipgrepRollbackObserved = $false
    try { Install-BundledRipgrep $InvalidRipgrepBundle $RipgrepBin }
    catch { $RipgrepRollbackObserved = $_.Exception.Message -like '*installed bundled ripgrep could not be verified*' }
    Assert $RipgrepRollbackObserved 'an invalid private ripgrep candidate did not fail final verification'
    Assert (Test-InstalledBundledRipgrep $RipgrepBin $RipgrepPackage $NewRevision) 'private ripgrep rollback did not restore the previous installation'
    Add-Content -LiteralPath (Join-Path $RipgrepBin 'bcodex-path\rg.exe') -Value 'tampered'
    Assert (-not (Test-InstalledBundledRipgrep $RipgrepBin $RipgrepPackage $NewRevision)) 'private ripgrep tampering was not detected'
    Install-BundledRipgrep $RipgrepBundle $RipgrepBin
    $RipgrepPackagePath = Join-Path $RipgrepBin 'bcodex-path'
    $RipgrepRecoveryId = [Guid]::NewGuid().ToString('N')
    Move-Item -LiteralPath (Join-Path $RipgrepPackagePath 'rg.exe') -Destination (Join-Path $RipgrepPackagePath ".rg-backup.$RipgrepRecoveryId.exe")
    Move-Item -LiteralPath (Join-Path $RipgrepPackagePath 'rg.json') -Destination (Join-Path $RipgrepPackagePath ".rg-backup.$RipgrepRecoveryId.json")
    [IO.File]::WriteAllText((Join-Path $RipgrepPackagePath 'rg.exe'), 'interrupted')
    [IO.File]::WriteAllText((Join-Path $RipgrepPackagePath 'rg.json'), '{}')
    Recover-BundledRipgrepArtifacts $RipgrepBin
    Assert (Test-InstalledBundledRipgrep $RipgrepBin $RipgrepPackage $NewRevision) 'private ripgrep recovery did not restore the previous installation'

    $BrokenFixture = Join-Path $TestRoot 'broken.exe'
    [IO.File]::WriteAllText($BrokenFixture, 'not an executable')
    $ReusableCandidate = Join-Path $TestRoot 'reusable-candidate.exe'
    $Reused = Try-StageReusableBinary `
        @($BrokenFixture, $ReusableFixture) $ReusableCandidate $NewRevision '0.1.2' $BuildInputHash
    Assert $Reused 'a damaged cache entry blocked verified executable reuse'
    Assert ((& $ReusableCandidate --internal-source-revision).Trim() -ceq $NewRevision) 'reused executable staged the wrong revision'
    Remove-Item -LiteralPath $ReusableCandidate -Force

    $ReusedMismatch = Try-StageReusableBinary `
        @($ReusableFixture) $ReusableCandidate $NewRevision '0.1.2' ('b' * 64)
    Assert (-not $ReusedMismatch) 'a mismatched build-input hash reused stale code'
    Assert (-not (Test-Path -LiteralPath $ReusableCandidate)) 'failed executable reuse retained a partial candidate'

    $EmptyFinalizer = Join-Path $TestRoot ('.bcodex-finalize.' + [Guid]::NewGuid().ToString('N'))
    [void](New-Item -ItemType Directory -Path $EmptyFinalizer)
    Recover-FinalizerArtifacts $TestRoot $Destination
    Assert (-not (Test-Path -LiteralPath $EmptyFinalizer)) 'recovery retained an empty completed transaction'

    Copy-Item -LiteralPath $OldFixture -Destination $Destination
    $CandidateHash = (Get-FileHash -LiteralPath $NewFixture -Algorithm SHA256).Hash.ToLowerInvariant()
    $Success = New-Transaction $TestRoot $Destination $NewFixture $NewRevision $CandidateHash
    if ($env:OS -ceq 'Windows_NT') {
        $SuccessManifest = [IO.File]::ReadAllText($Success.Manifest) | ConvertFrom-Json
        $SuccessManifest.destination = $SuccessManifest.destination.ToUpperInvariant()
        $SuccessManifest.candidate = $SuccessManifest.candidate.ToUpperInvariant()
        $SuccessManifest.backup = $SuccessManifest.backup.ToUpperInvariant()
        [IO.File]::WriteAllText(
            $Success.Manifest,
            ($SuccessManifest | ConvertTo-Json -Compress),
            (New-Object Text.UTF8Encoding($false))
        )
    }
    $Parent = Start-TestParent $PowerShellPath 3000
    $Processes.Add($Parent)
    $ParentTicks = $Parent.StartTime.ToUniversalTime().Ticks
    $FinalizeEnvironmentNames = @(
        'BCODEX_FINALIZE_MANIFEST',
        'BCODEX_FINALIZE_PARENT_PID',
        'BCODEX_FINALIZE_PARENT_TICKS'
    )
    $FinalizeEnvironment = Get-ProcessEnvironmentSnapshot
    Remove-Item -LiteralPath "Env:$($FinalizeEnvironmentNames[0])" -ErrorAction SilentlyContinue
    foreach ($Name in $FinalizeEnvironmentNames[1..2]) {
        [Environment]::SetEnvironmentVariable($Name, "sentinel-$Name", 'Process')
    }
    $ExpectedFinalizeEnvironment = Get-ProcessEnvironmentSnapshot
    try {
        $Finalizer = Start-DeferredReplacement $Success.Manifest $Parent.Id $ParentTicks $PowerShellPath
        foreach ($Name in $FinalizeEnvironmentNames) {
            $Value = [Environment]::GetEnvironmentVariable($Name, 'Process')
            if ($ExpectedFinalizeEnvironment.ContainsKey($Name)) {
                Assert ($Value -ceq $ExpectedFinalizeEnvironment[$Name]) "finalizer launch changed $Name"
            }
            else {
                Assert ($null -eq $Value) "finalizer launch leaked $Name"
            }
        }
    }
    finally {
        Restore-ProcessEnvironment $FinalizeEnvironment
    }
    $Processes.Add($Finalizer)
    Assert (Test-Path -LiteralPath $Success.Root) 'finalizer did not wait for the exact parent process'
    $FinalizerOwnsLock = $false
    $LockDeadline = [DateTime]::UtcNow.AddSeconds(2)
    while ([DateTime]::UtcNow -lt $LockDeadline -and -not $Parent.HasExited -and -not $Finalizer.HasExited) {
        $ProbeLock = $null
        try {
            $ProbeLock = New-Object IO.FileStream(
                (Join-Path $TestRoot '.bcodex-install.lock'),
                [IO.FileMode]::OpenOrCreate,
                [IO.FileAccess]::ReadWrite,
                [IO.FileShare]::None
            )
        }
        catch [IO.IOException] {
            $FinalizerOwnsLock = $true
        }
        finally {
            if ($null -ne $ProbeLock) { $ProbeLock.Dispose() }
        }
        if (-not $FinalizerOwnsLock) { Start-Sleep -Milliseconds 100 }
    }
    Assert $FinalizerOwnsLock 'finalizer did not protect its transaction while waiting for the parent'
    Assert ($Parent.WaitForExit(15000)) 'test parent did not exit'
    Assert ($Finalizer.WaitForExit(15000)) 'successful finalizer did not exit'
    Assert ($Finalizer.ExitCode -eq 0) 'successful finalizer returned a failure status'
    Assert (-not (Test-Path -LiteralPath $Success.Root)) 'successful finalizer retained its transaction'
    Assert ((& $Destination --internal-source-revision).Trim() -ceq $NewRevision) 'finalizer installed the wrong revision'

    Remove-Item -LiteralPath $Destination -Force
    Copy-Item -LiteralPath $OldFixture -Destination $Destination
    $Interrupted = New-Transaction $TestRoot $Destination $NewFixture $NewRevision $CandidateHash
    Move-Item -LiteralPath $Destination -Destination $Interrupted.Backup
    Move-Item -LiteralPath $Interrupted.Candidate -Destination $Destination
    Recover-FinalizerArtifacts $TestRoot $Destination
    Assert (-not (Test-Path -LiteralPath $Interrupted.Root)) 'recovery retained an interrupted committed transaction'
    Assert ((& $Destination --internal-source-revision).Trim() -ceq $OldRevision) 'recovery discarded the previous verified command'

    Remove-Item -LiteralPath $Destination -Force
    Copy-Item -LiteralPath $OldFixture -Destination $Destination
    $Failure = New-Transaction $TestRoot $Destination $NewFixture $NewRevision (('0' * 64) -join '')
    $FailureParent = Start-TestParent $PowerShellPath 1000
    $Processes.Add($FailureParent)
    $FailureTicks = $FailureParent.StartTime.ToUniversalTime().Ticks
    $FailedFinalizer = Start-DeferredReplacement $Failure.Manifest $FailureParent.Id $FailureTicks $PowerShellPath
    $Processes.Add($FailedFinalizer)
    Assert ($FailureParent.WaitForExit(15000)) 'failure-test parent did not exit'
    Assert ($FailedFinalizer.WaitForExit(15000)) 'failed finalizer did not exit'
    Assert ($FailedFinalizer.ExitCode -ne 0) 'digest failure returned success'
    Assert (Test-Path -LiteralPath $Failure.Manifest -PathType Leaf) 'failed finalizer discarded its recovery record'
    Assert (Test-Path -LiteralPath $Failure.Candidate -PathType Leaf) 'failed finalizer discarded its candidate evidence'
    Assert ((& $Destination --internal-source-revision).Trim() -ceq $OldRevision) 'digest failure changed the installed command'
    Recover-FinalizerArtifacts $TestRoot $Destination
    Assert (-not (Test-Path -LiteralPath $Failure.Root)) 'recovery did not clean a failed finalizer record'

    Remove-Item -LiteralPath $Destination -Force
    $RecoveryId = [Guid]::NewGuid().ToString('N')
    $RecoveryRoot = Join-Path $TestRoot ".bcodex-finalize.$RecoveryId"
    [void](New-Item -ItemType Directory -Path $RecoveryRoot)
    $RecoveryCandidate = Join-Path $RecoveryRoot 'candidate.exe'
    $RecoveryBackup = Join-Path $RecoveryRoot 'backup.exe'
    Copy-Item -LiteralPath $NewFixture -Destination $RecoveryCandidate
    Copy-Item -LiteralPath $OldFixture -Destination $RecoveryBackup
    $RecordedDestination = $Destination
    $RecordedCandidate = $RecoveryCandidate
    $RecordedBackup = $RecoveryBackup
    if ($env:OS -ceq 'Windows_NT') {
        $RecordedDestination = $RecordedDestination.ToUpperInvariant()
        $RecordedCandidate = $RecordedCandidate.ToUpperInvariant()
        $RecordedBackup = $RecordedBackup.ToUpperInvariant()
    }
    $RecoveryManifest = [ordered]@{
        transaction_id = $RecoveryId
        destination = $RecordedDestination
        candidate = $RecordedCandidate
        backup = $RecordedBackup
    }
    [IO.File]::WriteAllText(
        (Join-Path $RecoveryRoot 'transaction.json'),
        ($RecoveryManifest | ConvertTo-Json -Compress)
    )
    Recover-FinalizerArtifacts $TestRoot $Destination
    Assert (-not (Test-Path -LiteralPath $RecoveryRoot)) 'recovery retained a valid transaction record'
    Assert ((& $Destination --internal-source-revision).Trim() -ceq $OldRevision) 'recovery did not restore the backup'

    Write-Host 'Windows installer transaction tests passed.'
}
finally {
    foreach ($Process in $Processes) {
        if (-not $Process.HasExited) {
            $Process.Kill()
            [void]$Process.WaitForExit(5000)
        }
        $Process.Dispose()
    }
    if (Test-Path -LiteralPath $TestRoot) {
        Remove-Item -LiteralPath $TestRoot -Recurse -Force -ErrorAction SilentlyContinue
    }
}
