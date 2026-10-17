Set-StrictMode -Version Latest
$script:LiteCrPostgresBinDirectory = $null

function Stop-LiteCrOperation([string]$Code) {
    throw "liteCR operation failed [$Code]"
}

function Get-LiteCrCode([System.Exception]$Exception) {
    if ($Exception.Message -match '\[([A-Z0-9_]+)\]') { return $Matches[1] }
    return 'INTERNAL_ERROR'
}

function Assert-LiteCrNoReparsePath([string]$Path) {
    if ([string]::IsNullOrWhiteSpace($Path) -or -not [IO.Path]::IsPathRooted($Path)) { Stop-LiteCrOperation 'PATH_NOT_ABSOLUTE' }
    $full = [IO.Path]::GetFullPath($Path)
    $root = [IO.Path]::GetPathRoot($full)
    if (-not $root) { Stop-LiteCrOperation 'PATH_INVALID' }
    $cursor = $root
    $tail = $full.Substring($root.Length).TrimEnd([IO.Path]::DirectorySeparatorChar, [IO.Path]::AltDirectorySeparatorChar)
    foreach ($part in ($tail -split '[\\/]' | Where-Object { $_ })) {
        $cursor = Join-Path $cursor $part
        if (Test-Path -LiteralPath $cursor) {
            $item = Get-Item -LiteralPath $cursor -Force -ErrorAction Stop
            if (($item.Attributes -band [IO.FileAttributes]::ReparsePoint) -ne 0) { Stop-LiteCrOperation 'PATH_REPARSE_POINT' }
        }
    }
}

function Assert-LiteCrPrivatePath([string]$Path, [switch]$Directory) {
    Assert-LiteCrNoReparsePath $Path
    if (-not (Test-Path -LiteralPath $Path)) { Stop-LiteCrOperation 'PATH_MISSING' }
    $item = Get-Item -LiteralPath $Path -Force -ErrorAction Stop
    if ($Directory -and -not $item.PSIsContainer) { Stop-LiteCrOperation 'PATH_NOT_DIRECTORY' }
    if (-not $Directory -and $item.PSIsContainer) { Stop-LiteCrOperation 'PATH_NOT_FILE' }
    $acl = Get-Acl -LiteralPath $Path -ErrorAction Stop
    $currentSid = [Security.Principal.WindowsIdentity]::GetCurrent().User.Value
    $allowed = @($currentSid, 'S-1-5-18', 'S-1-5-32-544')
    try { $ownerSid = $acl.GetOwner([Security.Principal.SecurityIdentifier]).Value } catch { Stop-LiteCrOperation 'ACL_OWNER_UNREADABLE' }
    if ($ownerSid -notin $allowed) { Stop-LiteCrOperation 'ACL_OWNER' }
    foreach ($rule in $acl.Access) {
        if ($rule.AccessControlType -ne [Security.AccessControl.AccessControlType]::Allow) { continue }
        try { $sid = $rule.IdentityReference.Translate([Security.Principal.SecurityIdentifier]).Value } catch { Stop-LiteCrOperation 'ACL_IDENTITY_UNRESOLVED' }
        if ($sid -notin $allowed) { Stop-LiteCrOperation 'ACL_BROAD_ACCESS' }
    }
}

function Assert-LiteCrSecretFile([string]$Path) {
    Assert-LiteCrPrivatePath $Path
}

function Assert-LiteCrTrustedToolPath([string]$Path, [switch]$Directory) {
    Assert-LiteCrNoReparsePath $Path
    if (-not (Test-Path -LiteralPath $Path)) { Stop-LiteCrOperation 'POSTGRES_TOOL_MISSING' }
    $item = Get-Item -LiteralPath $Path -Force -ErrorAction Stop
    if ($Directory -and -not $item.PSIsContainer) { Stop-LiteCrOperation 'POSTGRES_BIN_NOT_DIRECTORY' }
    if (-not $Directory -and $item.PSIsContainer) { Stop-LiteCrOperation 'POSTGRES_TOOL_NOT_FILE' }
    $acl = Get-Acl -LiteralPath $Path -ErrorAction Stop
    $trusted = @([Security.Principal.WindowsIdentity]::GetCurrent().User.Value,'S-1-5-18','S-1-5-32-544')
    try { $trusted += ([Security.Principal.NTAccount]'NT SERVICE\TrustedInstaller').Translate([Security.Principal.SecurityIdentifier]).Value } catch { }
    $owner = $acl.GetOwner([Security.Principal.SecurityIdentifier]).Value
    if ($owner -notin $trusted) { Stop-LiteCrOperation 'POSTGRES_TOOL_OWNER' }
    $writeRights = [Security.AccessControl.FileSystemRights]::WriteData -bor
        [Security.AccessControl.FileSystemRights]::AppendData -bor
        [Security.AccessControl.FileSystemRights]::WriteExtendedAttributes -bor
        [Security.AccessControl.FileSystemRights]::WriteAttributes -bor
        [Security.AccessControl.FileSystemRights]::Delete -bor
        [Security.AccessControl.FileSystemRights]::ChangePermissions -bor
        [Security.AccessControl.FileSystemRights]::TakeOwnership -bor
        [Security.AccessControl.FileSystemRights]::DeleteSubdirectoriesAndFiles -bor
        [Security.AccessControl.FileSystemRights]::CreateFiles -bor
        [Security.AccessControl.FileSystemRights]::CreateDirectories
    foreach ($rule in $acl.Access) {
        if ($rule.AccessControlType -ne [Security.AccessControl.AccessControlType]::Allow) { continue }
        try { $sid = $rule.IdentityReference.Translate([Security.Principal.SecurityIdentifier]).Value } catch {
            $sid = switch -CaseSensitive ($rule.IdentityReference.Value) {
                'CREATOR OWNER' { 'S-1-3-0' }
                'APPLICATION PACKAGE AUTHORITY\ALL APPLICATION PACKAGES' { 'S-1-15-2-1' }
                'APPLICATION PACKAGE AUTHORITY\ALL RESTRICTED APPLICATION PACKAGES' { 'S-1-15-2-2' }
                default { Stop-LiteCrOperation 'POSTGRES_TOOL_ACL_IDENTITY' }
            }
        }
        if ($sid -notin $trusted -and (([int]$rule.FileSystemRights -band [int]$writeRights) -ne 0)) {
            Stop-LiteCrOperation 'POSTGRES_TOOL_ACL_WRITABLE'
        }
    }
}

function Set-LiteCrPostgresBinDirectory([string]$Path) {
    if ([string]::IsNullOrWhiteSpace($Path) -or -not [IO.Path]::IsPathRooted($Path)) { Stop-LiteCrOperation 'POSTGRES_BIN_PATH' }
    $full = [IO.Path]::GetFullPath($Path)
    Assert-LiteCrTrustedToolPath $full -Directory
    $script:LiteCrPostgresBinDirectory = $full
}

function Resolve-LiteCrNativeTool([string]$Name) {
    if (-not $script:LiteCrPostgresBinDirectory) { Stop-LiteCrOperation 'POSTGRES_BIN_NOT_CONFIGURED' }
    if ($Name -notin @('psql','pg_dump','pg_restore','createdb')) { Stop-LiteCrOperation 'POSTGRES_TOOL_NAME' }
    foreach ($extension in @('.exe','.cmd')) {
        $path = Join-Path $script:LiteCrPostgresBinDirectory ($Name + $extension)
        if (Test-Path -LiteralPath $path -PathType Leaf) {
            Assert-LiteCrTrustedToolPath $path
            return $path
        }
    }
    Stop-LiteCrOperation 'POSTGRES_TOOL_MISSING'
}

function Read-LiteCrConfig([string]$Path, [string[]]$AllowedKeys, [string[]]$RequiredKeys) {
    Assert-LiteCrPrivatePath $Path
    $result = @{}
    foreach ($line in [IO.File]::ReadAllLines($Path)) {
        if ($line.Length -eq 0 -or $line.StartsWith('#')) { continue }
        $index = $line.IndexOf('=')
        if ($index -lt 1) { Stop-LiteCrOperation 'CONFIG_SYNTAX' }
        $key = $line.Substring(0, $index)
        $value = $line.Substring($index + 1)
        if (-not $value -or $value.Contains("`r") -or $value.Contains("`n")) { Stop-LiteCrOperation 'CONFIG_VALUE' }
        if ($key -notin $AllowedKeys) { Stop-LiteCrOperation 'CONFIG_KEY_UNKNOWN' }
        if ($result.ContainsKey($key)) { Stop-LiteCrOperation 'CONFIG_KEY_DUPLICATE' }
        $result[$key] = $value
    }
    foreach ($key in $RequiredKeys) { if (-not $result.ContainsKey($key)) { Stop-LiteCrOperation 'CONFIG_KEY_MISSING' } }
    return $result
}

function Assert-LiteCrLibpqEnvironment {
    $deny = @('PGHOST','PGHOSTADDR','PGPORT','PGDATABASE','PGUSER','PGPASSWORD','PGSERVICE','PGSERVICEFILE',
      'PGSSLNEGOTIATION','PGREQUIREAUTH','PGSSLSNI','PGREQUIREPEER','PGGSSLIB','PGOPTIONS','PGCONNECT_TIMEOUT',
      'PGSSLMODE','PGREQUIRESSL','PGSSLCERT','PGSSLKEY','PGSSLROOTCERT','PGSSLCRL','PGSSLCRLDIR','PGSSLCOMPRESSION',
      'PGSSLCERTMODE','PGSSLMINPROTOCOLVERSION','PGSSLMAXPROTOCOLVERSION','PGSSLKEYLOGFILE','PGCHANNELBINDING',
      'PGGSSENCMODE','PGTARGETSESSIONATTRS','PGLOADBALANCEHOSTS','PGCLIENTENCODING','PGREALM','PGGSSDELEGATION',
      'PGKRBSRVNAME','PGDATESTYLE','PGTZ','PGGEQO','PGSYSCONFDIR','PGLOCALEDIR','PGAPPNAME')
    foreach ($name in $deny) {
        if (-not [string]::IsNullOrEmpty([Environment]::GetEnvironmentVariable($name))) { Stop-LiteCrOperation 'LIBPQ_ENVIRONMENT_OVERRIDE' }
    }
    $pass = [Environment]::GetEnvironmentVariable('PGPASSFILE')
    if ($pass) { Assert-LiteCrSecretFile $pass }
}

function Invoke-LiteCrNative([string]$Name, [string[]]$Arguments, [string]$ErrorCode) {
    $toolPath = Resolve-LiteCrNativeTool $Name
    & $toolPath @Arguments *> $null
    if ($LASTEXITCODE -ne 0) { Stop-LiteCrOperation $ErrorCode }
}

function Get-LiteCrNativeText([string]$Name, [string[]]$Arguments, [string]$ErrorCode) {
    $toolPath = Resolve-LiteCrNativeTool $Name
    $output = & $toolPath @Arguments 2>$null
    if ($LASTEXITCODE -ne 0) { Stop-LiteCrOperation $ErrorCode }
    return (($output -join "`n").Trim())
}

function Get-LiteCrSha256([string]$Path) {
    return (Get-FileHash -LiteralPath $Path -Algorithm SHA256).Hash.ToLowerInvariant()
}

function Write-LiteCrUtf8([string]$Path, [string]$Content) {
    $encoding = New-Object System.Text.UTF8Encoding($false)
    [IO.File]::WriteAllText($Path, $Content, $encoding)
}

Export-ModuleMember -Function Stop-LiteCrOperation, Get-LiteCrCode, Assert-LiteCrNoReparsePath, Assert-LiteCrPrivatePath, Assert-LiteCrSecretFile, Assert-LiteCrTrustedToolPath, Set-LiteCrPostgresBinDirectory, Read-LiteCrConfig, Assert-LiteCrLibpqEnvironment, Invoke-LiteCrNative, Get-LiteCrNativeText, Get-LiteCrSha256, Write-LiteCrUtf8
