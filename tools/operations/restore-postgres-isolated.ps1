[CmdletBinding()]
param(
    [Parameter(Mandatory=$true)][string]$Dump,
    [Parameter(Mandatory=$true)][string]$TargetDb,
    [Parameter(Mandatory=$true)][string]$IsolationMarker
)
$ErrorActionPreference = 'Stop'
Import-Module (Join-Path $PSScriptRoot 'BackupRestore.Common.psm1') -Force

try {
    Assert-LiteCrLibpqEnvironment
    if ($TargetDb -notmatch '^litecr_restore_[a-z0-9_]{1,48}$') { Stop-LiteCrOperation 'TARGET_NAME' }
    Assert-LiteCrPrivatePath $Dump
    Assert-LiteCrPrivatePath $IsolationMarker
    $markerValues = @{}
    foreach ($line in [IO.File]::ReadAllLines([IO.Path]::GetFullPath($IsolationMarker))) {
        $separator = $line.IndexOf('=')
        if ($separator -lt 1) { Stop-LiteCrOperation 'ISOLATION_MARKER_SYNTAX' }
        $key = $line.Substring(0,$separator); $value = $line.Substring($separator + 1)
        if ($key -notin @('format','system_identifier') -or $markerValues.ContainsKey($key)) { Stop-LiteCrOperation 'ISOLATION_MARKER_CONTENT' }
        $markerValues[$key] = $value
    }
    if ($markerValues.Count -ne 2 -or $markerValues.format -ne 'litecr-isolated-postgres-target-v2' -or $markerValues.system_identifier -notmatch '^\d{1,20}$') { Stop-LiteCrOperation 'ISOLATION_MARKER_CONTENT' }

    $configPath = [Environment]::GetEnvironmentVariable('LITECR_CONFIG_FILE')
    if (-not $configPath) { Stop-LiteCrOperation 'CONFIG_REQUIRED' }
    $cfg = Read-LiteCrConfig $configPath @('LITECR_PG_BIN_DIR','LITECR_PG_USER','LITECR_PGHOST','LITECR_PGPORT','LITECR_RESTORE_DB_OWNER') @('LITECR_PG_BIN_DIR','LITECR_PG_USER')
    Set-LiteCrPostgresBinDirectory $cfg.LITECR_PG_BIN_DIR
    if ($cfg.LITECR_PG_USER -notmatch '^[A-Za-z0-9_.-]{1,63}$') { Stop-LiteCrOperation 'DATABASE_USER' }
    $hostName = if ($cfg.LITECR_PGHOST) { $cfg.LITECR_PGHOST } else { '127.0.0.1' }
    if ($hostName -notin @('127.0.0.1','::1')) { Stop-LiteCrOperation 'RESTORE_HOST_MUST_BE_LOCAL' }
    if ($cfg.LITECR_PGPORT -and ($cfg.LITECR_PGPORT -notmatch '^\d{1,5}$' -or [int]$cfg.LITECR_PGPORT -lt 1 -or [int]$cfg.LITECR_PGPORT -gt 65535)) { Stop-LiteCrOperation 'DATABASE_PORT' }
    $owner = if ($cfg.LITECR_RESTORE_DB_OWNER) { $cfg.LITECR_RESTORE_DB_OWNER } else { $cfg.LITECR_PG_USER }
    if ($owner -notmatch '^[A-Za-z0-9_.-]{1,63}$') { Stop-LiteCrOperation 'DATABASE_OWNER' }
    $env:PGUSER = $cfg.LITECR_PG_USER
    $env:PGHOST = $hostName
    if ($cfg.LITECR_PGPORT) { $env:PGPORT = $cfg.LITECR_PGPORT }

    $dumpPath = [IO.Path]::GetFullPath($Dump)
    $directory = Split-Path -Parent $dumpPath
    $base = [IO.Path]::GetFileName($dumpPath)
    if ($base -notmatch '^(?<id>litecr-[A-Za-z0-9_.-]+-\d{8}T\d{6}Z-(?:\d+|[a-f0-9]{8}))\.dump$') { Stop-LiteCrOperation 'DUMP_PATH' }
    $id = $Matches.id
    $manifestPath = Join-Path $directory "$id.manifest"
    $checksumPath = Join-Path $directory "$id.sha256"
    $successPath = Join-Path $directory "$id.success"
    foreach ($p in @($manifestPath,$checksumPath,$successPath)) { Assert-LiteCrPrivatePath $p }

    $manifest = @{}
    foreach ($line in [IO.File]::ReadAllLines($manifestPath)) {
        $i = $line.IndexOf('=')
        if ($i -lt 1) { Stop-LiteCrOperation 'MANIFEST_SYNTAX' }
        $key = $line.Substring(0,$i); $value = $line.Substring($i+1)
        if ($manifest.ContainsKey($key)) { Stop-LiteCrOperation 'MANIFEST_DUPLICATE' }
        $manifest[$key] = $value
    }
    foreach ($key in @('format','dump_file','sha256','dump_bytes','schema_version','postgres_server_version')) { if (-not $manifest.ContainsKey($key)) { Stop-LiteCrOperation 'MANIFEST_FIELD_MISSING' } }
    if ($manifest.format -ne 'litecr-backup-v1') { Stop-LiteCrOperation 'MANIFEST_FORMAT' }
    if ($manifest.dump_file -ne $base -or $manifest.sha256 -notmatch '^[0-9a-f]{64}$' -or $manifest.dump_bytes -notmatch '^\d+$' -or $manifest.schema_version -notmatch '^\d+$' -or $manifest.postgres_server_version -notmatch '^\d+\.\d+') { Stop-LiteCrOperation 'MANIFEST_INVALID' }

    $success = @{}
    foreach ($line in [IO.File]::ReadAllLines($successPath)) {
        $i = $line.IndexOf('=')
        if ($i -lt 1) { Stop-LiteCrOperation 'SUCCESS_MARKER_SYNTAX' }
        $key = $line.Substring(0,$i); $value = $line.Substring($i+1)
        if ($key -notin @('format','backup_id','dump_file','sha256','completed_utc')) { Stop-LiteCrOperation 'SUCCESS_MARKER_KEY_UNKNOWN' }
        if ($success.ContainsKey($key)) { Stop-LiteCrOperation 'SUCCESS_MARKER_KEY_DUPLICATE' }
        $success[$key] = $value
    }
    if ($success.Count -ne 5 -or $success.format -ne 'litecr-backup-success-v1') { Stop-LiteCrOperation 'SUCCESS_MARKER_FORMAT' }
    if ($success.backup_id -ne $id -or $success.dump_file -ne $base -or $success.sha256 -ne $manifest.sha256 -or $success.completed_utc -notmatch '^\d{4}-\d\d-\d\dT\d\d:\d\d:\d\dZ$') { Stop-LiteCrOperation 'SUCCESS_MARKER_INVALID' }
    if ((Get-Item -LiteralPath $dumpPath).Length.ToString() -ne $manifest.dump_bytes) { Stop-LiteCrOperation 'DUMP_SIZE_MISMATCH' }
    $actualHash = Get-LiteCrSha256 $dumpPath
    if ($actualHash -ne $manifest.sha256) { Stop-LiteCrOperation 'DUMP_CHECKSUM_MISMATCH' }
    $checksumText = [IO.File]::ReadAllText($checksumPath).TrimEnd("`r","`n")
    if ($checksumText -cne "$($manifest.sha256)  $base") { Stop-LiteCrOperation 'CHECKSUM_SIDECAR_MISMATCH' }

    $targetVersion = Get-LiteCrNativeText 'psql' @('-X','-A','-t','--no-password','--dbname=postgres','-c','SHOW server_version') 'TARGET_VERSION_QUERY'
    if ($targetVersion -notmatch '^\d+\.\d+') { Stop-LiteCrOperation 'TARGET_VERSION_INVALID' }
    $targetSystemIdentifier = Get-LiteCrNativeText 'psql' @('-X','-A','-t','--no-password','--dbname=postgres','-c','SELECT system_identifier FROM pg_control_system()') 'TARGET_SYSTEM_IDENTIFIER_QUERY'
    if ($targetSystemIdentifier -notmatch '^\d{1,20}$' -or $targetSystemIdentifier -ne $markerValues.system_identifier) { Stop-LiteCrOperation 'TARGET_CLUSTER_IDENTITY_MISMATCH' }
    $sourceMajor = [int]($manifest.postgres_server_version.Split('.')[0])
    $targetMajor = [int]($targetVersion.Split('.')[0])
    if ($sourceMajor -ne $targetMajor) { Stop-LiteCrOperation 'POSTGRES_MAJOR_MISMATCH' }
    $exists = Get-LiteCrNativeText 'psql' @('-X','-A','-t','--no-password','--dbname=postgres','-c',"SELECT 1 FROM pg_database WHERE datname = '$TargetDb'") 'TARGET_DATABASE_QUERY'
    if ($exists -eq '1') { Stop-LiteCrOperation 'TARGET_DATABASE_ALREADY_EXISTS' }
    Invoke-LiteCrNative 'pg_restore' @('--list',$dumpPath) 'ARCHIVE_INVALID'

    Invoke-LiteCrNative 'createdb' @('--no-password','--maintenance-db=postgres',"--owner=$owner",$TargetDb) 'TARGET_DATABASE_CREATE'
    # Deliberately retain a newly-created target if restore fails; never drop data.
    Invoke-LiteCrNative 'pg_restore' @('--no-password','--exit-on-error','--single-transaction','--no-owner','--no-acl',"--dbname=$TargetDb",$dumpPath) 'PG_RESTORE_FAILED'
    $restored = Get-LiteCrNativeText 'psql' @('-X','-A','-t','--no-password',"--dbname=$TargetDb",'-c','SELECT version FROM litecr.schema_meta WHERE singleton = TRUE') 'RESTORED_SCHEMA_QUERY'
    if ($restored -ne $manifest.schema_version) { Stop-LiteCrOperation 'RESTORED_SCHEMA_VERSION_MISMATCH' }
    Write-Output "liteCR isolated restore completed: database=$TargetDb schema_version=$restored postgres_major=$targetMajor"
}
catch {
    $code = Get-LiteCrCode $_.Exception
    [Console]::Error.WriteLine("liteCR restore failed [$code]")
    exit 1
}
