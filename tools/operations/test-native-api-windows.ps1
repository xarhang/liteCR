param(
    [string]$Binary = "$(Split-Path -Parent $PSScriptRoot)\..\target\release\litecr.exe",
    [string]$PythonExe,
    [string]$PostgresBin = 'C:\Program Files\PostgreSQL\18\bin'
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

$Binary = [IO.Path]::GetFullPath($Binary)
$PostgresBin = [IO.Path]::GetFullPath($PostgresBin)

function Resolve-PythonExecutable {
    param([string]$RequestedPath)
    $candidates = @()
    if ($RequestedPath) { $candidates += $RequestedPath }
    if ($env:PYTHON) { $candidates += $env:PYTHON }
    $candidates += (Join-Path $PSScriptRoot '..\..\.venv\Scripts\python.exe')
    $candidates += (Join-Path $env:USERPROFILE '.cache\codex-runtimes\codex-primary-runtime\dependencies\python\python.exe')
    foreach ($installRoot in @((Join-Path $env:LOCALAPPDATA 'Programs\Python'), 'C:\Program Files\Python')) {
        if (Test-Path -LiteralPath $installRoot -PathType Container) {
            $candidates += Get-ChildItem -LiteralPath $installRoot -Directory -ErrorAction SilentlyContinue |
                Sort-Object Name -Descending |
                ForEach-Object { Join-Path $_.FullName 'python.exe' }
        }
    }
    $command = Get-Command python.exe -ErrorAction SilentlyContinue | Select-Object -First 1
    if ($command -and $command.Source -notmatch '\\Microsoft\\WindowsApps\\') { $candidates += $command.Source }

    foreach ($candidate in $candidates | Select-Object -Unique) {
        try {
            $resolved = (Resolve-Path -LiteralPath $candidate -ErrorAction Stop).Path
            if (-not (Test-Path -LiteralPath $resolved -PathType Leaf)) { continue }
            $null = & $resolved -c 'import http.server' 2>$null
            if ($LASTEXITCODE -eq 0) { return $resolved }
        } catch { continue }
    }
    throw 'A working Python 3 executable is required; pass -PythonExe or set PYTHON.'
}

$PythonExe = Resolve-PythonExecutable $PythonExe
foreach ($tool in @('initdb.exe', 'pg_ctl.exe', 'createdb.exe')) {
    if (-not (Test-Path -LiteralPath (Join-Path $PostgresBin $tool) -PathType Leaf)) {
        throw "Required isolated PostgreSQL tool is missing: $tool"
    }
}
if (-not (Test-Path -LiteralPath $Binary -PathType Leaf)) { throw "Release binary not found: $Binary" }

$tempRoot = [IO.Path]::GetFullPath($env:TEMP).TrimEnd('\')
$ownedRoot = Join-Path $tempRoot ('litecr-native-api-' + [guid]::NewGuid().ToString('N'))
$dataDir = Join-Path $ownedRoot 'postgres-data'
$pgLog = Join-Path $ownedRoot 'postgres.log'
$idpScript = Join-Path $ownedRoot 'synthetic-oidc.py'
$idpOut = Join-Path $ownedRoot 'oidc.stdout.log'
$idpErr = Join-Path $ownedRoot 'oidc.stderr.log'
$apiOut = Join-Path $ownedRoot 'api.stdout.log'
$apiErr = Join-Path $ownedRoot 'api.stderr.log'
$pgCtl = Join-Path $PostgresBin 'pg_ctl.exe'
$startedPg = $false
$idp = $null
$api = $null
$oldEnv = @{}
foreach ($name in @('LITECR_DATABASE_URL', 'LITECR_MIGRATION_DATABASE_URL', 'LITECR_ENV', 'LITECR_BIND', 'LITECR_OIDC_ISSUER_URL', 'LITECR_OIDC_CLIENT_ID', 'LITECR_OIDC_CLIENT_SECRET', 'LITECR_OIDC_REDIRECT_URI', 'LITECR_WEB_ORIGIN', 'LITECR_COOKIE_SECURE')) {
    $oldEnv[$name] = [Environment]::GetEnvironmentVariable($name, 'Process')
}

function Get-LoopbackPort {
    $socket = [Net.Sockets.TcpListener]::new([Net.IPAddress]::Loopback, 0)
    try { $socket.Start(); return ([Net.IPEndPoint]$socket.LocalEndpoint).Port }
    finally { $socket.Stop() }
}

function Invoke-Native {
    param([string]$File, [string[]]$Arguments, [string]$Failure)
    & $File @Arguments
    if ($LASTEXITCODE -ne 0) { throw $Failure }
}

try {
    New-Item -ItemType Directory -Path $ownedRoot | Out-Null
    Set-Content -LiteralPath (Join-Path $ownedRoot 'OWNER') -Value 'litecr-native-api-v1' -NoNewline
    $dbPort = Get-LoopbackPort
    $apiPort = Get-LoopbackPort
    $idpPort = Get-LoopbackPort
    $issuer = "http://127.0.0.1:$idpPort/realms/litecr"
    $baseUrl = "http://127.0.0.1:$apiPort"
    $dbUrl = "postgres://postgres@127.0.0.1:$dbPort/litecr_native_smoke?sslmode=disable"

    @'
import json, sys
from http.server import BaseHTTPRequestHandler, HTTPServer
issuer = sys.argv[1]
class Handler(BaseHTTPRequestHandler):
    def do_GET(self):
        if self.path.endswith('/protocol/openid-connect/certs'):
            body = b'{"keys": []}'
        elif self.path.endswith('/.well-known/openid-configuration'):
            body = json.dumps({
                'issuer': issuer,
                'authorization_endpoint': issuer + '/protocol/openid-connect/auth',
                'token_endpoint': issuer + '/protocol/openid-connect/token',
                'jwks_uri': issuer + '/protocol/openid-connect/certs',
                'response_types_supported': ['code'],
                'subject_types_supported': ['public'],
                'id_token_signing_alg_values_supported': ['RS256'],
            }).encode()
        else:
            self.send_error(404)
            return
        self.send_response(200)
        self.send_header('Content-Type', 'application/json')
        self.send_header('Content-Length', str(len(body)))
        self.end_headers()
        self.wfile.write(body)
    def log_message(self, *_): pass
HTTPServer(('127.0.0.1', int(issuer.split(':')[2].split('/')[0])), Handler).serve_forever()
'@ | Set-Content -LiteralPath $idpScript -Encoding utf8

    Invoke-Native (Join-Path $PostgresBin 'initdb.exe') @('-D', $dataDir, '-U', 'postgres', '--auth=trust', '--encoding=UTF8', '--no-locale') 'Disposable PostgreSQL initialization failed.'
    Invoke-Native $pgCtl @('-D', $dataDir, '-l', $pgLog, '-o', "-h 127.0.0.1 -p $dbPort -c listen_addresses=127.0.0.1", '-w', 'start') 'Disposable PostgreSQL startup failed.'
    $startedPg = $true
    Invoke-Native (Join-Path $PostgresBin 'createdb.exe') @('-h', '127.0.0.1', '-p', "$dbPort", '-U', 'postgres', 'litecr_native_smoke') 'Disposable PostgreSQL database creation failed.'

    $env:LITECR_MIGRATION_DATABASE_URL = $dbUrl
    Invoke-Native $Binary @('migrate') 'Release migration command failed.'
    $env:LITECR_DATABASE_URL = $dbUrl
    Invoke-Native $Binary @('doctor') 'Release doctor command failed.'

    $idp = Start-Process -FilePath $PythonExe -ArgumentList @($idpScript, $issuer) -WindowStyle Hidden -PassThru -RedirectStandardOutput $idpOut -RedirectStandardError $idpErr
    $env:LITECR_ENV = 'development'
    $env:LITECR_BIND = "127.0.0.1:$apiPort"
    $env:LITECR_OIDC_ISSUER_URL = $issuer
    $env:LITECR_OIDC_CLIENT_ID = 'synthetic-client'
    $env:LITECR_OIDC_CLIENT_SECRET = 'synthetic-secret'
    $env:LITECR_OIDC_REDIRECT_URI = "$baseUrl/api/v1/access/oidc/callback"
    $env:LITECR_WEB_ORIGIN = $baseUrl
    $env:LITECR_COOKIE_SECURE = 'false'
    $api = Start-Process -FilePath $Binary -ArgumentList @('serve') -WindowStyle Hidden -PassThru -RedirectStandardOutput $apiOut -RedirectStandardError $apiErr

    $ready = $false
    for ($attempt = 0; $attempt -lt 100; $attempt++) {
        if ($api.HasExited) { throw "Native API exited early; inspect $apiErr" }
        try {
            $response = Invoke-WebRequest -Uri "$baseUrl/health/ready" -UseBasicParsing -TimeoutSec 2
            if ($response.StatusCode -eq 200) { $ready = $true; break }
        } catch { Start-Sleep -Milliseconds 200 }
    }
    if (-not $ready) { throw "Native API readiness did not return HTTP 200; inspect $apiErr and $idpErr" }
    foreach ($path in @('/health/live', '/')) {
        $response = Invoke-WebRequest -Uri "$baseUrl$path" -UseBasicParsing -TimeoutSec 5
        if ($response.StatusCode -ne 200) { throw "Native API route failed: $path" }
    }
    $index = (Invoke-WebRequest -Uri "$baseUrl/" -UseBasicParsing -TimeoutSec 5).Content
    $assets = [regex]::Matches($index, '(?:src|href)="(/assets/[^\"]+\.js)"')
    if ($assets.Count -ne 1) { throw 'Embedded UI must reference exactly one hashed JavaScript entry asset.' }
    $assetResponse = Invoke-WebRequest -Uri "$baseUrl$($assets[0].Groups[1].Value)" -UseBasicParsing -TimeoutSec 10
    if ($assetResponse.StatusCode -ne 200 -or $assetResponse.RawContentLength -le 0) { throw 'Embedded JavaScript asset was not served.' }
    $styles = [regex]::Matches($index, '(?:src|href)="(/assets/[^"]+\.css)"')
    if ($styles.Count -ne 1) { throw 'Embedded UI must reference exactly one hashed CSS asset.' }
    $styleResponse = Invoke-WebRequest -Uri "$baseUrl$($styles[0].Groups[1].Value)" -UseBasicParsing -TimeoutSec 10
    if ($styleResponse.StatusCode -ne 200 -or $styleResponse.RawContentLength -le 0 -or $styleResponse.Content -notmatch '\.nav-icon') {
        throw 'Embedded visual-refresh CSS asset was missing or did not include the refreshed navigation styles.'
    }
    Write-Output 'Native Windows release smoke passed: schema migration, doctor, API readiness/liveness, embedded index and JavaScript/CSS assets (isolated PostgreSQL and synthetic OIDC only).'
}
finally {
    if ($api -and -not $api.HasExited) { Stop-Process -Id $api.Id -Force -ErrorAction SilentlyContinue }
    if ($idp -and -not $idp.HasExited) { Stop-Process -Id $idp.Id -Force -ErrorAction SilentlyContinue }
    if ($startedPg) {
        & $pgCtl -D $dataDir -m fast -w stop | Out-Null
        if ($LASTEXITCODE -ne 0) { throw 'Disposable PostgreSQL did not stop cleanly; preserved its private test directory.' }
    }
    foreach ($name in $oldEnv.Keys) {
        if ($null -eq $oldEnv[$name]) { [Environment]::SetEnvironmentVariable($name, $null, 'Process') }
        else { [Environment]::SetEnvironmentVariable($name, $oldEnv[$name], 'Process') }
    }
    if (Test-Path -LiteralPath $ownedRoot) {
        $resolvedRoot = [IO.Path]::GetFullPath($ownedRoot)
        $expectedPrefix = Join-Path $tempRoot 'litecr-native-api-'
        if ($resolvedRoot.StartsWith($expectedPrefix, [StringComparison]::OrdinalIgnoreCase) -and
            (Split-Path $resolvedRoot -Leaf) -match '^litecr-native-api-[0-9a-f]{32}$' -and
            (Test-Path -LiteralPath (Join-Path $resolvedRoot 'OWNER') -PathType Leaf) -and
            (Get-Content -LiteralPath (Join-Path $resolvedRoot 'OWNER') -Raw) -eq 'litecr-native-api-v1') {
            Remove-Item -LiteralPath $resolvedRoot -Recurse -Force
        } else { throw 'Refused to remove an unrecognized native API smoke directory.' }
    }
}
