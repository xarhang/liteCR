param([switch]$FullWorkspace, [string]$PythonExe)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
if (-not $env:CARGO_BUILD_JOBS) { $env:CARGO_BUILD_JOBS = '1' }

$postgresBin = if ($env:LITECR_TEST_POSTGRES_BIN) {
    [IO.Path]::GetFullPath($env:LITECR_TEST_POSTGRES_BIN)
} else {
    'C:\Program Files\PostgreSQL\18\bin'
}
$cargoExe = Join-Path $env:USERPROFILE '.cargo\bin\cargo.exe'
$mingwBin = 'C:\msys64\ucrt64\bin'
$toolchain = '1.85.0-x86_64-pc-windows-gnu'
foreach ($tool in @('initdb.exe', 'pg_ctl.exe', 'createdb.exe', 'psql.exe')) {
    if (-not (Test-Path (Join-Path $postgresBin $tool) -PathType Leaf)) {
        throw "Required isolated PostgreSQL tool is missing: $tool"
    }
}
if (-not (Test-Path $cargoExe -PathType Leaf) -or -not (Test-Path (Join-Path $mingwBin 'gcc.exe') -PathType Leaf)) {
    throw 'Windows GNU Rust toolchain dependencies are unavailable.'
}

$tempRoot = [IO.Path]::GetFullPath($env:TEMP).TrimEnd('\')
$ownedRoot = Join-Path $tempRoot ('litecr-registry-search-' + [guid]::NewGuid().ToString('N'))
$dataDir = Join-Path $ownedRoot 'data'
$logFile = Join-Path $ownedRoot 'postgres.log'
$pgCtl = Join-Path $postgresBin 'pg_ctl.exe'
$oldPath = $env:Path
$oldTestUrl = $env:LITECR_TEST_DATABASE_URL
$oldPython = $env:PYTHON
$listener = [Net.Sockets.TcpListener]::new([Net.IPAddress]::Loopback, 0)
$port = $null
$started = $false

function Invoke-Native {
    param([string]$File, [string[]]$Arguments, [string]$Failure)
    & $File @Arguments
    if ($LASTEXITCODE -ne 0) { throw $Failure }
}

try {
    New-Item -ItemType Directory -Path $ownedRoot | Out-Null
    Set-Content -LiteralPath (Join-Path $ownedRoot 'OWNER') -Value 'litecr-registry-search-v1' -NoNewline
    $listener.Start()
    $port = ([Net.IPEndPoint]$listener.LocalEndpoint).Port
    $listener.Stop()

    Invoke-Native (Join-Path $postgresBin 'initdb.exe') @(
        '-D', $dataDir, '-U', 'postgres', '--auth=trust', '--encoding=UTF8', '--no-locale'
    ) 'Disposable PostgreSQL initialization failed.'
    Invoke-Native $pgCtl @(
        '-D', $dataDir, '-l', $logFile, '-o', "-h 127.0.0.1 -p $port -c listen_addresses=127.0.0.1", '-w', 'start'
    ) 'Disposable loopback PostgreSQL startup failed.'
    $started = $true
    Invoke-Native (Join-Path $postgresBin 'createdb.exe') @(
        '-h', '127.0.0.1', '-p', "$port", '-U', 'postgres', 'litecr_test'
    ) 'Disposable PostgreSQL database creation failed.'

    $env:Path = "$mingwBin;$oldPath"
    $env:LITECR_TEST_DATABASE_URL = "postgres://postgres@127.0.0.1:$port/litecr_test?sslmode=disable"
    if ($PythonExe) {
        $resolvedPython = (Resolve-Path -LiteralPath $PythonExe -ErrorAction Stop).Path
        $env:PYTHON = $resolvedPython
    }
    Invoke-Native $cargoExe @(
        "+$toolchain", 'test', '-p', 'litecr-infrastructure', '--test', 'registry', '--locked', '--', '--test-threads=1'
    ) 'Registry PostgreSQL integration test failed.'
    # Each migration-sensitive integration binary gets a genuinely empty database.
    # Hand-editing SQLx/schema metadata left post-schema-17 columns behind as the
    # migration chain grew, causing clean migration tests to start from a hybrid schema.
    Invoke-Native (Join-Path $postgresBin 'dropdb.exe') @(
        '-h', '127.0.0.1', '-p', "$port", '-U', 'postgres', 'litecr_test'
    ) 'Disposable PostgreSQL database reset failed.'
    Invoke-Native (Join-Path $postgresBin 'createdb.exe') @(
        '-h', '127.0.0.1', '-p', "$port", '-U', 'postgres', 'litecr_test'
    ) 'Disposable PostgreSQL database recreation failed.'
    Invoke-Native $cargoExe @(
        "+$toolchain", 'test', '-p', 'litecr-infrastructure', '--test', 'foundation', '--locked', '--', '--test-threads=1'
    ) 'PostgreSQL foundation/index migration test failed.'
    Invoke-Native $cargoExe @(
        "+$toolchain", 'test', '-p', 'litecr-infrastructure', '--test', 'matching', '--locked', '--', '--test-threads=1'
    ) 'PostgreSQL matching/index integration tests failed.'
    if ($FullWorkspace) {
        # This regression deliberately requires an exact schema-22 fixture and is
        # run by the schema22 upgrade harness, not a shared already-migrated DB.
        Invoke-Native $cargoExe @(
            "+$toolchain", 'test', '--workspace', '--locked', '--', '--test-threads=1'
        ) 'Full Rust workspace test suite failed.'
    }
} finally {
    if ($listener.Server.IsBound) { $listener.Stop() }
    if ($started) {
        & $pgCtl -D $dataDir -m fast -w stop | Out-Null
        if ($LASTEXITCODE -ne 0) {
            throw 'Disposable PostgreSQL did not stop cleanly; preserved its private test directory.'
        }
    }
    $env:Path = $oldPath
    if ($null -eq $oldTestUrl) {
        Remove-Item Env:LITECR_TEST_DATABASE_URL -ErrorAction SilentlyContinue
    } else {
        $env:LITECR_TEST_DATABASE_URL = $oldTestUrl
    }
    if ($null -eq $oldPython) {
        Remove-Item Env:PYTHON -ErrorAction SilentlyContinue
    } else {
        $env:PYTHON = $oldPython
    }

    $resolvedRoot = [IO.Path]::GetFullPath($ownedRoot)
    $expectedPrefix = Join-Path $tempRoot 'litecr-registry-search-'
    if ($resolvedRoot.StartsWith($expectedPrefix, [StringComparison]::OrdinalIgnoreCase) -and
        (Split-Path $resolvedRoot -Leaf) -match '^litecr-registry-search-[0-9a-f]{32}$' -and
        (Test-Path (Join-Path $resolvedRoot 'OWNER') -PathType Leaf) -and
        (Get-Content -LiteralPath (Join-Path $resolvedRoot 'OWNER') -Raw) -eq 'litecr-registry-search-v1') {
        Remove-Item -LiteralPath $resolvedRoot -Recurse -Force
    } elseif (Test-Path $resolvedRoot) {
        throw 'Refused to remove an unrecognized PostgreSQL test directory.'
    }
}
