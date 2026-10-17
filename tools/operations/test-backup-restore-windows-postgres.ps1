[CmdletBinding()]
param([string]$PostgresBin = 'C:\Program Files\PostgreSQL\18\bin')

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
$repo = (Resolve-Path (Join-Path $PSScriptRoot '..\..')).Path
$PostgresBin = [IO.Path]::GetFullPath($PostgresBin)
$tools = @('initdb.exe','pg_ctl.exe','createdb.exe','dropdb.exe','psql.exe','pg_dump.exe','pg_restore.exe')
foreach ($tool in $tools) {
    if (-not (Test-Path -LiteralPath (Join-Path $PostgresBin $tool) -PathType Leaf)) {
        throw "Required PostgreSQL tool is missing: $tool"
    }
}

$tempRoot = [IO.Path]::GetFullPath($env:TEMP).TrimEnd('\')
$ownedRoot = Join-Path $tempRoot ('litecr-backup-restore-pg18-' + [guid]::NewGuid().ToString('N'))
$dataDir = Join-Path $ownedRoot 'data'
$backupDir = Join-Path $ownedRoot 'backups'
$pgLog = Join-Path $ownedRoot 'postgres.log'
$passwordFile = Join-Path $ownedRoot 'initdb-password.txt'
$passFile = Join-Path $ownedRoot 'postgres.pass'
$wrongPassFile = Join-Path $ownedRoot 'wrong-postgres.pass'
$missingPassFile = Join-Path $ownedRoot 'missing-postgres.pass'
$pgCtl = Join-Path $PostgresBin 'pg_ctl.exe'
$started = $false
$listener = [Net.Sockets.TcpListener]::new([Net.IPAddress]::Loopback, 0)
$port = $null
$oldPath = $env:PATH
$oldEnvironment = @{}
foreach ($name in @('LITECR_CONFIG_FILE','PGPASSFILE','PGHOST','PGHOSTADDR','PGPORT','PGDATABASE','PGUSER','PGPASSWORD','PGSERVICE','PGSERVICEFILE','PGSSLNEGOTIATION','PGREQUIREAUTH','PGSSLSNI','PGREQUIREPEER','PGGSSLIB','PGOPTIONS','PGCONNECT_TIMEOUT','PGSSLMODE','PGREQUIRESSL','PGSSLCERT','PGSSLKEY','PGSSLROOTCERT','PGSSLCRL','PGSSLCRLDIR','PGSSLCOMPRESSION','PGSSLCERTMODE','PGSSLMINPROTOCOLVERSION','PGSSLMAXPROTOCOLVERSION','PGSSLKEYLOGFILE','PGCHANNELBINDING','PGGSSENCMODE','PGTARGETSESSIONATTRS','PGLOADBALANCEHOSTS','PGCLIENTENCODING','PGREALM','PGGSSDELEGATION','PGKRBSRVNAME','PGDATESTYLE','PGTZ','PGGEQO','PGSYSCONFDIR','PGLOCALEDIR','PGAPPNAME','LITECR_PG_DATABASE','LITECR_PG_USER','LITECR_PGHOST','LITECR_PGPORT')) {
    $oldEnvironment[$name] = [Environment]::GetEnvironmentVariable($name, 'Process')
}

function Invoke-Native([string]$File, [string[]]$Arguments, [string]$Failure) {
    & $File @Arguments
    if ($LASTEXITCODE -ne 0) { throw $Failure }
}

function Protect-Path([string]$Path, [switch]$Directory) {
    $currentSid = [Security.Principal.WindowsIdentity]::GetCurrent().User.Value
    if ($Directory) {
        & icacls.exe $Path /inheritance:r /grant:r ("*{0}:(OI)(CI)F" -f $currentSid) '*S-1-5-18:(OI)(CI)F' '*S-1-5-32-544:(OI)(CI)F' | Out-Null
    } else {
        & icacls.exe $Path /inheritance:r /grant:r ("*{0}:F" -f $currentSid) '*S-1-5-18:F' '*S-1-5-32-544:F' | Out-Null
    }
    if ($LASTEXITCODE -ne 0) { throw "Could not protect owned test path: $Path" }
}

function Invoke-Script([string]$Path, [string[]]$Arguments) {
    $childPowerShell = Join-Path $PSHOME 'powershell.exe'
    $previous = $ErrorActionPreference
    $ErrorActionPreference = 'Continue'
    $output = & $childPowerShell -NoProfile -ExecutionPolicy Bypass -File $Path @Arguments 2>&1
    $code = $LASTEXITCODE
    $ErrorActionPreference = $previous
    return @{ Code=$code; Output=($output -join "`n") }
}

function Write-PrivateText([string]$Path, [string]$Content) {
    [IO.File]::WriteAllText($Path, $Content, [Text.UTF8Encoding]::new($false))
    Protect-Path $Path
}

function Assert-AuthenticationDenied([string]$Label) {
    $result = Invoke-Script (Join-Path $PSScriptRoot 'backup-postgres.ps1') @('-Mode','--archive-only')
    if ($result.Code -eq 0) { throw "$Label authentication unexpectedly succeeded." }
    if ($result.Output.Contains($testPassword)) { throw "$Label authentication output exposed a credential." }
    if ($wrongPassword -and $result.Output.Contains($wrongPassword)) { throw "$Label authentication output exposed a credential." }
    if ($result.Output -notmatch 'liteCR backup failed \[(PATH_MISSING|SCHEMA_VERSION_QUERY|SERVER_VERSION_QUERY|PG_DUMP_FAILED|INTERNAL_ERROR)\]') {
        $safeOutput = $result.Output
        if ($testPassword) { $safeOutput = $safeOutput.Replace($testPassword,'<redacted>') }
        if ($wrongPassword) { $safeOutput = $safeOutput.Replace($wrongPassword,'<redacted>') }
        throw "$Label authentication did not return the expected redacted backup failure: $safeOutput"
    }
}

try {
    New-Item -ItemType Directory -Path $ownedRoot | Out-Null
    Set-Content -LiteralPath (Join-Path $ownedRoot 'OWNER') -Value 'litecr-backup-restore-pg18-v1' -NoNewline -Encoding Ascii
    Protect-Path $ownedRoot -Directory
    New-Item -ItemType Directory -Path $backupDir | Out-Null
    Protect-Path $backupDir -Directory
    $listener.Start()
    $port = ([Net.IPEndPoint]$listener.LocalEndpoint).Port
    $listener.Stop()

    $env:PATH = "$PostgresBin;$oldPath"
    foreach ($name in @('LITECR_CONFIG_FILE','PGPASSFILE','PGHOST','PGHOSTADDR','PGPORT','PGDATABASE','PGUSER','PGPASSWORD','PGSERVICE','PGSERVICEFILE')) {
        Remove-Item "Env:$name" -ErrorAction SilentlyContinue
    }
    $random = [Security.Cryptography.RandomNumberGenerator]::Create()
    try {
        $passwordBytes = New-Object byte[] 32
        $random.GetBytes($passwordBytes)
        $testPassword = [BitConverter]::ToString($passwordBytes).Replace('-','')
        [Array]::Clear($passwordBytes,0,$passwordBytes.Length)
    } finally { $random.Dispose() }
    Write-PrivateText $passwordFile ($testPassword + "`n")
    Invoke-Native (Join-Path $PostgresBin 'initdb.exe') @('-D',$dataDir,'-U','postgres','--auth-local=scram-sha-256','--auth-host=scram-sha-256',"--pwfile=$passwordFile",'--encoding=UTF8','--no-locale') 'Disposable PostgreSQL initdb failed.'
    Invoke-Native $pgCtl @('-D',$dataDir,'-l',$pgLog,'-o',"-h 127.0.0.1 -p $port -c listen_addresses=127.0.0.1",'-w','start') 'Disposable PostgreSQL start failed.'
    $started = $true
    Write-PrivateText $passFile (("127.0.0.1:{0}:*:postgres:{1}`n" -f $port,$testPassword))
    $wrongRandom = [Security.Cryptography.RandomNumberGenerator]::Create()
    try {
        $wrongPasswordBytes = New-Object byte[] 32
        $wrongRandom.GetBytes($wrongPasswordBytes)
        $wrongPassword = [BitConverter]::ToString($wrongPasswordBytes).Replace('-','')
        [Array]::Clear($wrongPasswordBytes,0,$wrongPasswordBytes.Length)
    } finally { $wrongRandom.Dispose() }
    Write-PrivateText $wrongPassFile (("127.0.0.1:{0}:*:postgres:{1}`n" -f $port,$wrongPassword))
    [Environment]::SetEnvironmentVariable('PGPASSFILE',$passFile,'Process')
    Invoke-Native (Join-Path $PostgresBin 'createdb.exe') @('-h','127.0.0.1','-p',"$port",'-U','postgres','litecr_backup_source') 'Synthetic source database creation failed.'
    $sql = "CREATE SCHEMA litecr; CREATE TABLE litecr.schema_meta (singleton boolean PRIMARY KEY DEFAULT TRUE CHECK (singleton), version bigint NOT NULL); INSERT INTO litecr.schema_meta(version) VALUES (23); CREATE TABLE litecr.backup_probe (probe_id integer PRIMARY KEY, probe_value text NOT NULL); INSERT INTO litecr.backup_probe VALUES (1, 'synthetic-backup-restore-check');"
    Invoke-Native (Join-Path $PostgresBin 'psql.exe') @('-X','-v','ON_ERROR_STOP=1','-h','127.0.0.1','-p',"$port",'-U','postgres','-d','litecr_backup_source','-c',$sql) 'Synthetic schema fixture setup failed.'

    $backupConfig = Join-Path $ownedRoot 'backup.conf'
    @("LITECR_PG_BIN_DIR=$PostgresBin",'LITECR_PG_DATABASE=litecr_backup_source','LITECR_PG_USER=postgres',"LITECR_BACKUP_DIR=$backupDir",'LITECR_RELEASE_ID=windows-pg18-synthetic','LITECR_PGHOST=127.0.0.1',"LITECR_PGPORT=$port",'LITECR_MIN_FREE_KB=1') | Set-Content -LiteralPath $backupConfig -Encoding Ascii
    Protect-Path $backupConfig
    $env:LITECR_CONFIG_FILE = $backupConfig

    [Environment]::SetEnvironmentVariable('PGPASSFILE',$missingPassFile,'Process')
    Assert-AuthenticationDenied 'Missing passfile'
    Write-Output 'PASS missing password file is denied with redacted output'
    [Environment]::SetEnvironmentVariable('PGPASSFILE',$wrongPassFile,'Process')
    Assert-AuthenticationDenied 'Wrong password'
    Write-Output 'PASS incorrect password is denied with redacted output'
    [Environment]::SetEnvironmentVariable('PGPASSFILE',$passFile,'Process')

    $backup = Invoke-Script (Join-Path $PSScriptRoot 'backup-postgres.ps1') @('-Mode','--archive-only')
    if ($backup.Code -ne 0) { throw "Windows PostgreSQL backup failed: $($backup.Output)" }
    $dump = Get-ChildItem -LiteralPath $backupDir -Filter '*.dump' | Select-Object -First 1
    if (-not $dump) { throw 'Backup completed without publishing a dump.' }

    $marker = Join-Path $ownedRoot 'isolation.marker'
    $systemIdentifier = & (Join-Path $PostgresBin 'psql.exe') -X -A -t --no-password -h 127.0.0.1 -p "$port" -U postgres -d postgres -c 'SELECT system_identifier FROM pg_control_system()'
    if ($LASTEXITCODE -ne 0 -or ($systemIdentifier -join '').Trim() -notmatch '^\d{1,20}$') { throw 'Could not read the disposable target cluster system identifier.' }
    @('format=litecr-isolated-postgres-target-v2',"system_identifier=$(($systemIdentifier -join '').Trim())") | Set-Content -LiteralPath $marker -Encoding Ascii
    Protect-Path $marker
    $restoreConfig = Join-Path $ownedRoot 'restore.conf'
    @("LITECR_PG_BIN_DIR=$PostgresBin",'LITECR_PG_USER=postgres','LITECR_PGHOST=127.0.0.1',"LITECR_PGPORT=$port",'LITECR_RESTORE_DB_OWNER=postgres') | Set-Content -LiteralPath $restoreConfig -Encoding Ascii
    Protect-Path $restoreConfig
    $env:LITECR_CONFIG_FILE = $restoreConfig
    $restore = Invoke-Script (Join-Path $PSScriptRoot 'restore-postgres-isolated.ps1') @('-Dump',$dump.FullName,'-TargetDb','litecr_restore_pg18_probe','-IsolationMarker',$marker)
    if ($restore.Code -ne 0) { throw "Windows PostgreSQL isolated restore failed: $($restore.Output)" }

    $version = & (Join-Path $PostgresBin 'psql.exe') -X -A -t --no-password -h 127.0.0.1 -p "$port" -U postgres -d litecr_restore_pg18_probe -c 'SELECT version FROM litecr.schema_meta WHERE singleton = TRUE'
    if ($LASTEXITCODE -ne 0 -or ($version -join '').Trim() -ne '23') { throw 'Restored schema version did not reconcile.' }
    $probe = & (Join-Path $PostgresBin 'psql.exe') -X -A -t --no-password -h 127.0.0.1 -p "$port" -U postgres -d litecr_restore_pg18_probe -c 'SELECT probe_value FROM litecr.backup_probe WHERE probe_id = 1'
    if ($LASTEXITCODE -ne 0 -or ($probe -join '').Trim() -ne 'synthetic-backup-restore-check') { throw 'Restored synthetic probe row did not reconcile.' }
    Write-Output 'Windows PostgreSQL 18.3 backup/isolated-restore integration passed: schema and synthetic row reconciled on a private loopback cluster.'
    Write-Output 'LIMITATION=SCRAM-auth synthetic cluster; no production credentials, service, data, off-host recovery, Windows Server, or RPO/RTO acceptance'
}
finally {
    if ($listener.Server.IsBound) { $listener.Stop() }
    if (Test-Path -LiteralPath $dataDir) {
        & $pgCtl -D $dataDir status *> $null
        if ($LASTEXITCODE -eq 0) {
            & $pgCtl -D $dataDir -m fast -w stop | Out-Null
            if ($LASTEXITCODE -ne 0) { throw 'Disposable PostgreSQL did not stop cleanly; its owned test directory is preserved.' }
        }
    }
    $env:PATH = $oldPath
    foreach ($name in $oldEnvironment.Keys) {
        if ($null -eq $oldEnvironment[$name]) { [Environment]::SetEnvironmentVariable($name,$null,'Process') }
        else { [Environment]::SetEnvironmentVariable($name,$oldEnvironment[$name],'Process') }
    }
    if (Test-Path -LiteralPath $ownedRoot) {
        $resolvedRoot = [IO.Path]::GetFullPath($ownedRoot)
        $expectedPrefix = Join-Path $tempRoot 'litecr-backup-restore-pg18-'
        $valid = $resolvedRoot.StartsWith($expectedPrefix,[StringComparison]::OrdinalIgnoreCase) -and
            (Split-Path $resolvedRoot -Leaf) -match '^litecr-backup-restore-pg18-[0-9a-f]{32}$' -and
            (Test-Path -LiteralPath (Join-Path $resolvedRoot 'OWNER') -PathType Leaf) -and
            (Get-Content -LiteralPath (Join-Path $resolvedRoot 'OWNER') -Raw) -eq 'litecr-backup-restore-pg18-v1'
        if ($valid) { Remove-Item -LiteralPath $resolvedRoot -Recurse -Force }
        else { throw 'Refused to remove an unrecognized PostgreSQL backup/restore test directory.' }
    }
}
