param(
    [string]$HarnessPath = (Join-Path $PSScriptRoot '..\target\x86_64-pc-windows-gnu\debug\litecr-mcp-synthetic-harness.exe')
)

$ErrorActionPreference = 'Stop'
$requiredInspector = '@modelcontextprotocol/inspector@2.10.1'
$node = Get-Command node.exe -ErrorAction Stop
$nodeVersionText = (& $node.Source --version).Trim()
if ($LASTEXITCODE -ne 0 -or $nodeVersionText -notmatch '^v(\d+)\.(\d+)\.(\d+)') {
    throw "Could not read Node.js version from $($node.Source)."
}
$nodeVersion = [version]::Parse("$($Matches[1]).$($Matches[2]).$($Matches[3])")
if ($nodeVersion -lt [version]'22.19.0') {
    throw "Inspector 2.10.1 requires Node.js >=22.19.0; found $nodeVersionText."
}

$harness = (Resolve-Path -LiteralPath $HarnessPath -ErrorAction Stop).Path
$expectedHash = '92DA0CA283A3257C1CA9DBA1D1470FC24F112CDDEFB43338A32B80DD2C0CAB40'
$actualHash = (Get-FileHash -LiteralPath $harness -Algorithm SHA256).Hash
if ($actualHash -ne $expectedHash) {
    throw "Harness SHA-256 mismatch. Expected $expectedHash; found $actualHash."
}

$runRoot = Join-Path $env:TEMP ('litecr-inspector-smoke-' + [guid]::NewGuid().ToString('N'))
$null = New-Item -ItemType Directory -Path $runRoot
$environmentNames = @('npm_config_cache', 'HOME', 'USERPROFILE', 'APPDATA', 'LOCALAPPDATA')
$savedEnvironment = @{}
foreach ($name in $environmentNames) {
    $savedEnvironment[$name] = [Environment]::GetEnvironmentVariable($name, 'Process')
}
try {
$env:npm_config_cache = Join-Path $runRoot 'npm-cache'
$env:HOME = $runRoot
$env:USERPROFILE = $runRoot
$env:APPDATA = Join-Path $runRoot 'AppData\Roaming'
$env:LOCALAPPDATA = Join-Path $runRoot 'AppData\Local'
$null = New-Item -ItemType Directory -Force -Path $env:APPDATA, $env:LOCALAPPDATA

function Invoke-InspectorMethod([string]$Method, [string[]]$ExtraArgs = @()) {
    $stderrFile = Join-Path $runRoot ($Method.Replace('/', '-') + '.stderr.txt')
    $arguments = @(
        'exec', '--yes', '--package', $requiredInspector, '--', 'inspector', '--cli',
        $harness, '--transport', 'stdio', '--method', $Method, '--format', 'json',
        '--connect-timeout', '10000'
    ) + $ExtraArgs
    $stdoutLines = @(& npm.cmd @arguments 2> $stderrFile)
    $exitCode = $LASTEXITCODE
    $stderr = if (Test-Path -LiteralPath $stderrFile) { Get-Content -Raw -LiteralPath $stderrFile } else { '' }
    $stdout = $stdoutLines -join "`n"
    if ($exitCode -ne 0) {
        throw "Inspector $Method failed (exit $exitCode). stdout=$stdout`nstderr=$stderr"
    }
    try { $response = $stdout | ConvertFrom-Json -ErrorAction Stop }
    catch { throw "Inspector $Method stdout was not one JSON response: $stdout`nstderr=$stderr" }
    [pscustomobject]@{ Method = $Method; ExitCode = $exitCode; Stdout = $stdout; Stderr = $stderr; Response = $response }
}

$initialize = Invoke-InspectorMethod 'initialize'
if (-not $initialize.Response.result.protocolVersion) {
    throw "Initialize response omitted negotiated protocolVersion: $($initialize.Stdout)"
}

$list = Invoke-InspectorMethod 'tools/list'
$tools = @($list.Response.result.tools)
if ($tools.Count -ne 1 -or $tools[0].name -ne 'litecr.synthetic_demo') {
    throw "Expected only litecr.synthetic_demo; received $($list.Stdout)"
}

$call = Invoke-InspectorMethod 'tools/call' @('--tool-name', 'litecr.synthetic_demo', '--tool-args-json', '{}')
$textBlocks = @($call.Response.result.content | Where-Object { $_.type -eq 'text' } | ForEach-Object text)
if ($textBlocks -notcontains 'Synthetic demo only. No registry data was read or changed.') {
    throw "Synthetic tool response did not match the fixed canary: $($call.Stdout)"
}

Write-Output "Inspector package: $requiredInspector"
Write-Output "Node: $nodeVersionText ($($node.Source))"
Write-Output "Harness: $harness"
Write-Output "Harness SHA-256: $actualHash"
Write-Output "Negotiated MCP protocol: $($initialize.Response.result.protocolVersion)"
foreach ($result in @($initialize, $list, $call)) {
    Write-Output "[$($result.Method)] exit=$($result.ExitCode) stdout=$($result.Stdout)"
    Write-Output "[$($result.Method)] stderr=$($result.Stderr)"
}
Write-Output 'Temporary npm cache and profile were removed after the smoke run.'
}
finally {
    foreach ($name in $environmentNames) {
        [Environment]::SetEnvironmentVariable($name, $savedEnvironment[$name], 'Process')
    }

    if (Test-Path -LiteralPath $runRoot) {
        $resolvedRunRoot = (Resolve-Path -LiteralPath $runRoot).Path
        $resolvedTempRoot = [IO.Path]::GetFullPath($env:TEMP).TrimEnd([IO.Path]::DirectorySeparatorChar) + [IO.Path]::DirectorySeparatorChar
        $runLeaf = Split-Path -Leaf $resolvedRunRoot
        if ($resolvedRunRoot.StartsWith($resolvedTempRoot, [StringComparison]::OrdinalIgnoreCase) -and
            $runLeaf -match '^litecr-inspector-smoke-[0-9a-f]{32}$') {
            Remove-Item -LiteralPath $resolvedRunRoot -Recurse -Force
        }
        else {
            throw 'Refusing to remove the Inspector smoke path because it is outside its exact task-owned temporary directory.'
        }
    }
}
