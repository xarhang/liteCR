[CmdletBinding()]
param([ValidateSet('--archive-only')][string]$Mode = '--archive-only')
$ErrorActionPreference = 'Stop'
Import-Module (Join-Path $PSScriptRoot 'BackupRestore.Common.psm1') -Force

try {
    Assert-LiteCrLibpqEnvironment
    $configPath = [Environment]::GetEnvironmentVariable('LITECR_CONFIG_FILE')
    if (-not $configPath) { Stop-LiteCrOperation 'CONFIG_REQUIRED' }
    $cfg = Read-LiteCrConfig $configPath @('LITECR_PG_BIN_DIR','LITECR_PG_DATABASE','LITECR_PG_USER','LITECR_BACKUP_DIR','LITECR_RELEASE_ID','LITECR_PGHOST','LITECR_PGPORT','LITECR_MIN_FREE_KB') @('LITECR_PG_BIN_DIR','LITECR_PG_DATABASE','LITECR_PG_USER','LITECR_BACKUP_DIR','LITECR_RELEASE_ID')
    Set-LiteCrPostgresBinDirectory $cfg.LITECR_PG_BIN_DIR
    if ($cfg.LITECR_PG_DATABASE -notmatch '^[A-Za-z0-9_.-]{1,63}$') { Stop-LiteCrOperation 'DATABASE_NAME' }
    if ($cfg.LITECR_PG_USER -notmatch '^[A-Za-z0-9_.-]{1,63}$') { Stop-LiteCrOperation 'DATABASE_USER' }
    if ($cfg.LITECR_RELEASE_ID -notmatch '^[A-Za-z0-9_.+-]{1,128}$') { Stop-LiteCrOperation 'RELEASE_ID' }
    if ($cfg.LITECR_PGPORT -and ($cfg.LITECR_PGPORT -notmatch '^\d{1,5}$' -or [int]$cfg.LITECR_PGPORT -lt 1 -or [int]$cfg.LITECR_PGPORT -gt 65535)) { Stop-LiteCrOperation 'DATABASE_PORT' }
    if ($cfg.LITECR_PGHOST) { $env:PGHOST = $cfg.LITECR_PGHOST }
    if ($cfg.LITECR_PGPORT) { $env:PGPORT = $cfg.LITECR_PGPORT }
    $env:PGDATABASE = $cfg.LITECR_PG_DATABASE
    $env:PGUSER = $cfg.LITECR_PG_USER

    $backupDir = [IO.Path]::GetFullPath($cfg.LITECR_BACKUP_DIR)
    # Validate every existing ancestor before creation so a configured junction
    # is rejected before New-Item can follow it.
    Assert-LiteCrNoReparsePath $backupDir
    if (-not (Test-Path -LiteralPath $backupDir)) { [void](New-Item -ItemType Directory -Path $backupDir -Force) }
    Assert-LiteCrPrivatePath $backupDir -Directory
    $minKb = 1048576L
    if ($cfg.LITECR_MIN_FREE_KB) {
        if ($cfg.LITECR_MIN_FREE_KB -notmatch '^\d{1,12}$') { Stop-LiteCrOperation 'FREE_SPACE_CONFIG' }
        $minKb = [long]$cfg.LITECR_MIN_FREE_KB
    }
    $drive = New-Object IO.DriveInfo([IO.Path]::GetPathRoot($backupDir))
    if ($drive.AvailableFreeSpace -lt ($minKb * 1KB)) { Stop-LiteCrOperation 'INSUFFICIENT_FREE_SPACE' }

    $lockPath = Join-Path $backupDir '.backup.lock'
    $lock = $null
    try { $lock = [IO.File]::Open($lockPath, [IO.FileMode]::OpenOrCreate, [IO.FileAccess]::ReadWrite, [IO.FileShare]::None) }
    catch { Stop-LiteCrOperation 'BACKUP_ALREADY_RUNNING' }
    try {
        $schemaBefore = Get-LiteCrNativeText 'psql' @('-X','-A','-t','--no-password',"--dbname=$($cfg.LITECR_PG_DATABASE)",'-c','SELECT version FROM litecr.schema_meta WHERE singleton = TRUE') 'SCHEMA_VERSION_QUERY'
        if ($schemaBefore -notmatch '^\d+$') { Stop-LiteCrOperation 'SCHEMA_VERSION_MISSING' }
        $serverBefore = Get-LiteCrNativeText 'psql' @('-X','-A','-t','--no-password',"--dbname=$($cfg.LITECR_PG_DATABASE)",'-c','SHOW server_version') 'SERVER_VERSION_QUERY'
        if ($serverBefore -notmatch '^\d+\.\d+([ \t]+\([A-Za-z0-9 ._+()\-]+\))?$') { Stop-LiteCrOperation 'SERVER_VERSION_INVALID' }
        $stamp = [DateTime]::UtcNow.ToString('yyyyMMddTHHmmssZ')
        $id = "litecr-$($cfg.LITECR_PG_DATABASE)-$stamp-$([Guid]::NewGuid().ToString('N').Substring(0,8))"
        $dumpName = "$id.dump"
        $finalDump = Join-Path $backupDir $dumpName
        $finalManifest = Join-Path $backupDir "$id.manifest"
        $finalChecksum = Join-Path $backupDir "$id.sha256"
        $finalSuccess = Join-Path $backupDir "$id.success"
        foreach ($p in @($finalDump,$finalManifest,$finalChecksum,$finalSuccess)) { if (Test-Path -LiteralPath $p) { Stop-LiteCrOperation 'ARTIFACT_COLLISION' } }
        $suffix = [Guid]::NewGuid().ToString('N')
        $tmpDump = Join-Path $backupDir ".litecr-$suffix.dump.tmp"
        $tmpManifest = Join-Path $backupDir ".litecr-$suffix.manifest.tmp"
        $tmpChecksum = Join-Path $backupDir ".litecr-$suffix.sha256.tmp"
        $tmpSuccess = Join-Path $backupDir ".litecr-$suffix.success.tmp"
        $published = $false
        try {
            Invoke-LiteCrNative 'pg_dump' @('--no-password','--format=custom','--no-owner','--no-acl',"--dbname=$($cfg.LITECR_PG_DATABASE)","--file=$tmpDump") 'PG_DUMP_FAILED'
            if (-not (Test-Path -LiteralPath $tmpDump) -or (Get-Item -LiteralPath $tmpDump).Length -lt 1) { Stop-LiteCrOperation 'EMPTY_DUMP' }
            Invoke-LiteCrNative 'pg_restore' @('--list',$tmpDump) 'ARCHIVE_INVALID'
            $schemaAfter = Get-LiteCrNativeText 'psql' @('-X','-A','-t','--no-password',"--dbname=$($cfg.LITECR_PG_DATABASE)",'-c','SELECT version FROM litecr.schema_meta WHERE singleton = TRUE') 'SCHEMA_VERSION_QUERY'
            if ($schemaAfter -ne $schemaBefore) { Stop-LiteCrOperation 'SCHEMA_CHANGED_DURING_BACKUP' }
            $serverAfter = Get-LiteCrNativeText 'psql' @('-X','-A','-t','--no-password',"--dbname=$($cfg.LITECR_PG_DATABASE)",'-c','SHOW server_version') 'SERVER_VERSION_QUERY'
            if ($serverAfter -ne $serverBefore) { Stop-LiteCrOperation 'SERVER_CHANGED_DURING_BACKUP' }
            $bytes = (Get-Item -LiteralPath $tmpDump).Length
            $hash = Get-LiteCrSha256 $tmpDump
            $dumpVersion = Get-LiteCrNativeText 'pg_dump' @('--version') 'PG_DUMP_VERSION'
            if ($dumpVersion -notmatch '^pg_dump \(PostgreSQL\) [0-9]+\.[0-9]+') { Stop-LiteCrOperation 'PG_DUMP_VERSION_INVALID' }
            $created = [DateTime]::UtcNow.ToString("yyyy-MM-dd'T'HH:mm:ss'Z'")
            $manifest = @("format=litecr-backup-v1","created_utc=$created","database=$($cfg.LITECR_PG_DATABASE)","release_id=$($cfg.LITECR_RELEASE_ID)","schema_version=$schemaAfter","postgres_server_version=$serverAfter","pg_dump_version=$dumpVersion","dump_file=$dumpName","dump_bytes=$bytes","sha256=$hash") -join "`n"
            $successAt = [DateTime]::UtcNow.ToString("yyyy-MM-dd'T'HH:mm:ss'Z'")
            $success = @('format=litecr-backup-success-v1',"backup_id=$id","dump_file=$dumpName","sha256=$hash","completed_utc=$successAt") -join "`n"
            Write-LiteCrUtf8 $tmpManifest ($manifest + "`n")
            Write-LiteCrUtf8 $tmpChecksum ("$hash  $dumpName`n")
            Write-LiteCrUtf8 $tmpSuccess ($success + "`n")
            [IO.File]::Move($tmpDump,$finalDump)
            [IO.File]::Move($tmpManifest,$finalManifest)
            [IO.File]::Move($tmpChecksum,$finalChecksum)
            $published = $true
            [IO.File]::Move($tmpSuccess,$finalSuccess)
            Write-Output "liteCR backup success marker: $([IO.Path]::GetFileName($finalSuccess))"
            Write-Output "liteCR backup created: $dumpName (schema $schemaAfter; $bytes bytes)"
        }
        finally {
            foreach ($p in @($tmpDump,$tmpManifest,$tmpChecksum,$tmpSuccess)) { if (Test-Path -LiteralPath $p) { Remove-Item -LiteralPath $p -Force } }
            if (-not $published) { foreach ($p in @($finalDump,$finalManifest,$finalChecksum,$finalSuccess)) { if (Test-Path -LiteralPath $p) { Remove-Item -LiteralPath $p -Force } } }
        }
    }
    finally { if ($lock) { $lock.Dispose() } }
}
catch {
    $code = Get-LiteCrCode $_.Exception
    [Console]::Error.WriteLine("liteCR backup failed [$code]")
    exit 1
}
