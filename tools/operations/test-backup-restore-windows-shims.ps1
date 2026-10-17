$ErrorActionPreference = 'Stop'
$repo = (Resolve-Path (Join-Path $PSScriptRoot '..\..')).Path
$operations = Join-Path $repo 'tools\operations'
$psExe = Join-Path $PSHOME 'powershell.exe'
$root = Join-Path $env:TEMP ('litecr-ps-shim-' + [Guid]::NewGuid().ToString('N'))
$shim = Join-Path $root 'bin'
$attackerBin = Join-Path $root 'ambient-bin'
$data = Join-Path $root 'data'
$log = Join-Path $root 'native.log'
$restoreDbLog = Join-Path $root 'createdb.log'
$oldPath = $env:PATH
$oldConfig = $env:LITECR_CONFIG_FILE
$oldPass = $env:PGPASSFILE
$oldMajor = $env:SHIM_PG_MAJOR
$oldSystemIdentifier = $env:SHIM_SYSTEM_IDENTIFIER
$oldFail = $env:SHIM_FAIL_RESTORE
$oldFailDump = $env:SHIM_FAIL_DUMP
$oldDbExists = $env:SHIM_DB_EXISTS
$oldLog = $env:SHIM_LOG
$passed = 0
$heldBackupLock = $null

function Assert-That([bool]$Condition, [string]$Message) { if (-not $Condition) { throw "ASSERTION_FAILED: $Message" } }
function Protect-Tree([string]$Path) {
    $sid = [Security.Principal.WindowsIdentity]::GetCurrent().User.Value
    $item = Get-Item -LiteralPath $Path -Force
    if ($item.PSIsContainer) {
        & icacls.exe $Path /reset /T /C | Out-Null
        & icacls.exe $Path /inheritance:r /grant:r ("*{0}:(OI)(CI)F" -f $sid) '*S-1-5-18:(OI)(CI)F' '*S-1-5-32-544:(OI)(CI)F' | Out-Null
    } else {
        & icacls.exe $Path /reset | Out-Null
        & icacls.exe $Path /inheritance:r /grant:r ("*{0}:F" -f $sid) '*S-1-5-18:F' '*S-1-5-32-544:F' | Out-Null
    }
    if ($LASTEXITCODE -ne 0) { throw 'Could not set private synthetic ACL' }
}
function Run-Backup {
    $previous = $ErrorActionPreference
    $ErrorActionPreference = 'Continue'
    $output = & $psExe -NoProfile -ExecutionPolicy Bypass -File (Join-Path $operations 'backup-postgres.ps1') -Mode '--archive-only' 2>&1
    $code = $LASTEXITCODE
    $ErrorActionPreference = $previous
    return @{ Code=$code; Output=($output -join "`n") }
}
function Run-Restore([string]$Dump, [string]$Target, [string]$Marker) {
    $previous = $ErrorActionPreference
    $ErrorActionPreference = 'Continue'
    $output = & $psExe -NoProfile -ExecutionPolicy Bypass -File (Join-Path $operations 'restore-postgres-isolated.ps1') -Dump $Dump -TargetDb $Target -IsolationMarker $Marker 2>&1
    $code = $LASTEXITCODE
    $ErrorActionPreference = $previous
    return @{ Code=$code; Output=($output -join "`n") }
}
function Pass([string]$Name) { $script:passed++; Write-Output "PASS $Name" }

try {
    [void](New-Item -ItemType Directory -Path $root)
    [void](New-Item -ItemType Directory -Path $shim)
    [void](New-Item -ItemType Directory -Path $attackerBin)
    [void](New-Item -ItemType Directory -Path $data)
    Protect-Tree $root
    Protect-Tree $shim
    Protect-Tree $attackerBin
    Protect-Tree $data
    $env:SHIM_LOG = $log
    $env:PATH = "$attackerBin;$shim;$oldPath"
    $env:SHIM_PG_MAJOR = '18'
    $env:SHIM_SYSTEM_IDENTIFIER = '7451100000000000001'
    $env:SHIM_FAIL_RESTORE = '0'
    $env:SHIM_FAIL_DUMP = '0'
    $env:SHIM_DB_EXISTS = '0'

    @'
@echo off
echo psql %*>>"%SHIM_LOG%"
echo %*| findstr /i /c:"SHOW server_version" >nul
if not errorlevel 1 (echo %SHIM_PG_MAJOR%.6&exit /b 0)
echo %*| findstr /i /c:"SELECT system_identifier FROM pg_control_system()" >nul
if not errorlevel 1 (echo %SHIM_SYSTEM_IDENTIFIER%&exit /b 0)
echo %*| findstr /i /c:"SELECT version FROM litecr.schema_meta" >nul
if not errorlevel 1 (echo 23&exit /b 0)
echo %*| findstr /i /c:"FROM pg_database" >nul
if not errorlevel 1 (if "%SHIM_DB_EXISTS%"=="1" (echo 1)&exit /b 0)
exit /b 0
'@ | Set-Content -LiteralPath (Join-Path $shim 'psql.cmd') -Encoding Ascii
    @'
@echo off
powershell.exe -NoProfile -ExecutionPolicy Bypass -File "%~dp0pg_dump-shim.ps1" %*
exit /b %errorlevel%
'@ | Set-Content -LiteralPath (Join-Path $shim 'pg_dump.cmd') -Encoding Ascii
    @'
$argsText = $args -join ' '
Add-Content -LiteralPath $env:SHIM_LOG -Value "pg_dump $argsText"
if ($args -contains '--version') { Write-Output 'pg_dump (PostgreSQL) 18.6'; exit 0 }
if ($env:SHIM_FAIL_DUMP -eq '1') { exit 9 }
$fileArg = $args | Where-Object { $_ -like '--file=*' } | Select-Object -First 1
if (-not $fileArg) { exit 4 }
$path = $fileArg.Substring(7)
[IO.File]::WriteAllText($path, 'synthetic-custom-archive')
exit 0
'@ | Set-Content -LiteralPath (Join-Path $shim 'pg_dump-shim.ps1') -Encoding Ascii
    @'
@echo off
echo pg_restore %*>>"%SHIM_LOG%"
echo %*| findstr /i /c:"--list" >nul
if not errorlevel 1 exit /b 0
if "%SHIM_FAIL_RESTORE%"=="1" exit /b 7
exit /b 0
'@ | Set-Content -LiteralPath (Join-Path $shim 'pg_restore.cmd') -Encoding Ascii
    @'
@echo off
echo createdb %*>>"%SHIM_LOG%"
echo %*>>"%RESTORE_DB_LOG%"
exit /b 0
'@ | Set-Content -LiteralPath (Join-Path $shim 'createdb.cmd') -Encoding Ascii
    @'
@echo off
echo injected-path>>"%SHIM_LOG%"
exit /b 9
'@ | Set-Content -LiteralPath (Join-Path $attackerBin 'psql.cmd') -Encoding Ascii
    $env:RESTORE_DB_LOG = $restoreDbLog

    $backupConfig = Join-Path $root 'backup.conf'
    @("LITECR_PG_BIN_DIR=$shim","LITECR_PG_DATABASE=litecr","LITECR_PG_USER=litecr_backup","LITECR_BACKUP_DIR=$data","LITECR_RELEASE_ID=0.1.0-test","LITECR_PGPORT=5432","LITECR_MIN_FREE_KB=1") | Set-Content -LiteralPath $backupConfig -Encoding Ascii
    Protect-Tree $backupConfig
    $env:LITECR_CONFIG_FILE = $backupConfig
    $backup = Run-Backup
    Assert-That ($backup.Code -eq 0) "synthetic backup failed: $($backup.Output); shimlog=$(if(Test-Path $log){Get-Content $log -Raw})"
    $dump = Get-ChildItem -LiteralPath $data -Filter '*.dump' | Select-Object -First 1
    Assert-That ($null -ne $dump) 'backup dump published'
    $success = Get-ChildItem -LiteralPath $data -Filter '*.success' | Select-Object -First 1
    Assert-That ($null -ne $success) 'success marker published last'
    $hash = (Get-FileHash -LiteralPath $dump.FullName -Algorithm SHA256).Hash.ToLowerInvariant()
    Assert-That ((Get-Content -LiteralPath (Join-Path $data ($dump.BaseName + '.manifest')) -Raw).Contains("sha256=$hash")) 'manifest digest matches dump'
    Assert-That ((Get-Content -LiteralPath $success.FullName -Raw).Contains("sha256=$hash")) 'success marker digest matches dump'
    Assert-That (-not (Select-String -LiteralPath $log -Pattern 'injected-path' -Quiet)) 'ambient PATH PostgreSQL shim was never executed'
    Pass 'backup publishes matching manifest/checksum/success marker set'

    $artifactCount = @(Get-ChildItem -LiteralPath $data -File | Where-Object Name -Like 'litecr-*').Count
    $lockPath = Join-Path $data '.backup.lock'
    $heldBackupLock = [IO.File]::Open($lockPath,[IO.FileMode]::OpenOrCreate,[IO.FileAccess]::ReadWrite,[IO.FileShare]::None)
    $concurrentBackup = Run-Backup
    Assert-That ($concurrentBackup.Code -ne 0 -and $concurrentBackup.Output.Contains('BACKUP_ALREADY_RUNNING')) 'concurrent backup is rejected by exclusive lock'
    Assert-That (@(Get-ChildItem -LiteralPath $data -File | Where-Object Name -Like 'litecr-*').Count -eq $artifactCount) 'concurrent backup publishes no additional artifacts'
    $heldBackupLock.Dispose()
    $heldBackupLock = $null
    Pass 'overlapping Windows backup is rejected without publishing another set'

    $env:SHIM_FAIL_DUMP = '1'
    $failedBackup = Run-Backup
    Assert-That ($failedBackup.Code -ne 0 -and $failedBackup.Output.Contains('PG_DUMP_FAILED')) 'failed dump is reported'
    Assert-That (@(Get-ChildItem -LiteralPath $data -File | Where-Object Name -Like 'litecr-*').Count -eq $artifactCount) 'failed backup publishes no partial set or success marker'
    $env:SHIM_FAIL_DUMP = '0'
    Pass 'failed dump leaves no partially published backup artifacts'

    $marker = Join-Path $root 'isolated.marker'
    @('format=litecr-isolated-postgres-target-v2',"system_identifier=$env:SHIM_SYSTEM_IDENTIFIER") | Set-Content -LiteralPath $marker -Encoding Ascii
    Protect-Tree $marker
    $restoreConfig = Join-Path $root 'restore.conf'
    @("LITECR_PG_BIN_DIR=$shim",'LITECR_PG_USER=litecr_restore_admin','LITECR_PGHOST=127.0.0.1','LITECR_PGPORT=5432','LITECR_RESTORE_DB_OWNER=litecr_runtime') | Set-Content -LiteralPath $restoreConfig -Encoding Ascii
    Protect-Tree $restoreConfig
    $env:LITECR_CONFIG_FILE = $restoreConfig
    $restore = Run-Restore $dump.FullName 'litecr_restore_good' $marker
    Assert-That ($restore.Code -eq 0) "synthetic isolated restore failed: $($restore.Output)"
    Pass 'restore validates artifact and same-major local target before creating a new DB'

    $before = (Get-Content -LiteralPath $restoreDbLog -Raw)
    $dumpBytes = [IO.File]::ReadAllBytes($dump.FullName)
    [IO.File]::AppendAllText($dump.FullName, 'tampered')
    $tamperedDump = Run-Restore $dump.FullName 'litecr_restore_baddump' $marker
    Assert-That ($tamperedDump.Code -ne 0 -and $tamperedDump.Output.Contains('DUMP_SIZE_MISMATCH')) 'tampered dump is rejected'
    Assert-That ((Get-Content -LiteralPath $restoreDbLog -Raw) -eq $before) 'tampered dump rejected before database creation'
    [IO.File]::WriteAllBytes($dump.FullName, $dumpBytes)
    Pass 'tampered dump is rejected before any database creation'

    $env:SHIM_DB_EXISTS = '1'
    $existing = Run-Restore $dump.FullName 'litecr_restore_existing' $marker
    Assert-That ($existing.Code -ne 0 -and $existing.Output.Contains('TARGET_DATABASE_ALREADY_EXISTS')) 'existing target is rejected'
    Assert-That ((Get-Content -LiteralPath $restoreDbLog -Raw) -eq $before) 'existing target is never passed to createdb'
    $env:SHIM_DB_EXISTS = '0'
    Pass 'existing restore target is refused without a create/drop command'
    $successPath = Join-Path $data ($dump.BaseName + '.success')
    $successText = Get-Content -LiteralPath $successPath -Raw
    [IO.File]::WriteAllText($successPath, [regex]::Replace($successText,'sha256=[0-9a-f]{64}',('sha256=' + ('0' * 64))))
    $badMarker = Run-Restore $dump.FullName 'litecr_restore_badmarker' $marker
    Assert-That ($badMarker.Code -ne 0 -and $badMarker.Output.Contains('SUCCESS_MARKER_INVALID')) "tampered success marker rejected: $($badMarker.Output)"
    Assert-That ((Get-Content -LiteralPath $restoreDbLog -Raw) -eq $before) 'invalid marker rejected before createdb'
    Pass 'invalid success marker is rejected before any database creation'
    [IO.File]::WriteAllText($successPath,$successText)

    $env:SHIM_SYSTEM_IDENTIFIER = '7451100000000000002'
    $wrongCluster = Run-Restore $dump.FullName 'litecr_restore_wrongcluster' $marker
    Assert-That ($wrongCluster.Code -ne 0 -and $wrongCluster.Output.Contains('TARGET_CLUSTER_IDENTITY_MISMATCH')) 'marker for a different PostgreSQL cluster is rejected'
    Assert-That ((Get-Content -LiteralPath $restoreDbLog -Raw) -eq $before) 'wrong cluster identity rejected before createdb'
    $env:SHIM_SYSTEM_IDENTIFIER = '7451100000000000001'
    Pass 'target PostgreSQL system identifier must match the isolation marker'

    $env:SHIM_PG_MAJOR = '17'
    $major = Run-Restore $dump.FullName 'litecr_restore_wrongmajor' $marker
    Assert-That ($major.Code -ne 0 -and $major.Output.Contains('POSTGRES_MAJOR_MISMATCH')) 'different PG major rejected'
    Assert-That ((Get-Content -LiteralPath $restoreDbLog -Raw) -eq $before) 'major mismatch rejected before createdb'
    $env:SHIM_PG_MAJOR = '18'
    Pass 'cross-major restore rejected before database creation'

    $remoteConfig = Join-Path $root 'remote.conf'
    @("LITECR_PG_BIN_DIR=$shim",'LITECR_PG_USER=litecr_restore_admin','LITECR_PGHOST=198.51.100.7') | Set-Content -LiteralPath $remoteConfig -Encoding Ascii
    Protect-Tree $remoteConfig
    $env:LITECR_CONFIG_FILE = $remoteConfig
    $remote = Run-Restore $dump.FullName 'litecr_restore_remote' $marker
    Assert-That ($remote.Code -ne 0 -and $remote.Output.Contains('RESTORE_HOST_MUST_BE_LOCAL')) 'remote restore host rejected'
    Pass 'restore refuses non-loopback target host'

    $env:LITECR_CONFIG_FILE = $restoreConfig
    & icacls.exe $restoreConfig /grant '*S-1-5-32-545:R' | Out-Null
    $broadAcl = Run-Restore $dump.FullName 'litecr_restore_broadacl' $marker
    Assert-That ($broadAcl.Code -ne 0 -and $broadAcl.Output.Contains('ACL_BROAD_ACCESS')) 'broad config ACL rejected'
    Assert-That ((Get-Content -LiteralPath $restoreDbLog -Raw) -eq $before) 'unsafe config ACL rejected before createdb'
    Protect-Tree $restoreConfig
    Pass 'restore refuses config files readable by the broad BUILTIN Users group'

    $env:SHIM_FAIL_RESTORE = '1'
    $failedRestore = Run-Restore $dump.FullName 'litecr_restore_failed' $marker
    Assert-That ($failedRestore.Code -ne 0 -and $failedRestore.Output.Contains('PG_RESTORE_FAILED')) 'failed restore reported'
    $afterFailure = Get-Content -LiteralPath $restoreDbLog -Raw
    Assert-That ($afterFailure.Contains('litecr_restore_failed')) 'failed target left available for inspection'
    Assert-That (-not (Select-String -LiteralPath $log -Pattern 'dropdb|DROP DATABASE' -Quiet)) 'no destructive drop command issued'
    Pass 'restore failure never drops or replaces the newly-created target'

    $badConfig = Join-Path $root 'bad.conf'
    @(("LITECR_PG_BIN_DIR=" + $shim),'LITECR_PG_DATABASE=litecr','LITECR_PG_USER=litecr_backup',('LITECR_BACKUP_DIR=' + $data),'LITECR_RELEASE_ID=0.1.0-test','UNKNOWN_KEY=bad') | Set-Content -LiteralPath $badConfig -Encoding Ascii
    Protect-Tree $badConfig
    $env:LITECR_CONFIG_FILE = $badConfig
    $badCfg = Run-Backup
    Assert-That ($badCfg.Code -ne 0 -and $badCfg.Output.Contains('CONFIG_KEY_UNKNOWN')) "unknown config key rejected: $($badCfg.Output); config=$([IO.File]::ReadAllText($badConfig))"
    Pass 'strict backup config rejects unknown keys'
    Write-Output "Windows synthetic PowerShell backup/restore shims passed: $passed checks; no PostgreSQL service was used."
}
finally {
    if ($heldBackupLock) { $heldBackupLock.Dispose() }
    $env:PATH = $oldPath
    foreach ($name in @('LITECR_CONFIG_FILE','PGPASSFILE','SHIM_PG_MAJOR','SHIM_SYSTEM_IDENTIFIER','SHIM_FAIL_RESTORE','SHIM_FAIL_DUMP','SHIM_DB_EXISTS','SHIM_LOG','RESTORE_DB_LOG')) {
        $old = switch ($name) { 'LITECR_CONFIG_FILE' {$oldConfig} 'PGPASSFILE' {$oldPass} 'SHIM_PG_MAJOR' {$oldMajor} 'SHIM_SYSTEM_IDENTIFIER' {$oldSystemIdentifier} 'SHIM_FAIL_RESTORE' {$oldFail} 'SHIM_FAIL_DUMP' {$oldFailDump} 'SHIM_DB_EXISTS' {$oldDbExists} 'SHIM_LOG' {$oldLog} 'RESTORE_DB_LOG' {$null} }
        if ($null -eq $old) { [Environment]::SetEnvironmentVariable($name,$null) } else { [Environment]::SetEnvironmentVariable($name,$old) }
    }
    if (Test-Path -LiteralPath $root) {
        & icacls.exe $root /reset /T /C | Out-Null
        Remove-Item -LiteralPath $root -Recurse -Force
    }
}
