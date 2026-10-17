param(
  [Parameter(Mandatory = $true)]
  [ValidatePattern('^[a-f0-9]{16}$')]
  [string] $RunId,
  [Parameter(Mandatory = $true)]
  [ValidatePattern('^[A-Za-z]{2,8}(-[A-Za-z0-9]{1,8})*$')]
  [string] $Locale,
  [Parameter()]
  [switch] $ReviewerOnly,
  [Parameter()]
  [switch] $FocusedUi,
  [Parameter()]
  [switch] $RegistryOnly
)

$ErrorActionPreference = 'Stop'
$databaseName = "litecr_admin_e2e_$RunId"
$fixtureDirectory = "\\wsl.localhost\Ubuntu\home\xarhang\.litecr-dev\admin-e2e\$RunId"
$repoRoot = (Resolve-Path (Join-Path $PSScriptRoot '..\..\..\..\..')).Path
$webRoot = Join-Path $repoRoot 'apps\web'
$node = (Get-Command node.exe -ErrorAction Stop).Source
$viteScript = Join-Path $webRoot 'node_modules\vite\bin\vite.js'
$npm = (Get-Command npm.cmd -ErrorAction Stop).Source
$runnerScript = Join-Path $webRoot 'tests\fixtures\admin\capture-admin-e2e-sessions.mjs'
$reviewSeedScript = Join-Path $webRoot 'tests\fixtures\admin\seed-review-e2e.mjs'
$playwrightCli = Join-Path $webRoot 'node_modules\@playwright\test\cli.js'
$specPath = Join-Path $webRoot 'tests\e2e\admin\workflows.spec.ts'
$reviewSpecPath = Join-Path $webRoot 'tests\e2e\review\reviewer-workspace.spec.ts'
$registrySpecPath = Join-Path $webRoot 'tests\e2e\registry\identity-workflow.spec.ts'
$accessibilitySpecPath = Join-Path $webRoot 'tests\e2e\accessibility\ui-quality.spec.ts'
$baseUrl = 'http://127.0.0.1:4173'
$runFolder = Join-Path $fixtureDirectory 'windows-runner'
$viteProcess = $null
$sessionProcess = $null
$seedProcess = $null
$playwrightProcess = $null
$testExit = 1
$cleanupErrors = [System.Collections.Generic.List[string]]::new()

if (@($ReviewerOnly, $FocusedUi, $RegistryOnly).Where({ $_ }).Count -gt 1) {
  throw 'Choose only one focused fixture mode: -ReviewerOnly, -FocusedUi, or -RegistryOnly.'
}

function Get-SafeLogSummary([string] $Path) {
  if (-not (Test-Path -LiteralPath $Path -PathType Leaf)) { return '' }
  $lines = Get-Content -LiteralPath $Path -ErrorAction SilentlyContinue | Select-Object -First 20
  $safe = foreach ($line in $lines) {
    if ($line -match '(?i)password|client.secret|access.token|authorization|cookie') {
      '[sensitive diagnostic line omitted]'
    } else {
      $line -replace 'https?://[^\s?]+\?[^\s]+', '[URL query redacted]'
    }
  }
  return ($safe -join ' ')
}

if (-not (Test-Path -LiteralPath $viteScript -PathType Leaf) -or
    -not (Test-Path -LiteralPath $runnerScript -PathType Leaf) -or
    -not (Test-Path -LiteralPath $reviewSeedScript -PathType Leaf) -or
    -not (Test-Path -LiteralPath $playwrightCli -PathType Leaf) -or
    -not (Test-Path -LiteralPath $specPath -PathType Leaf) -or
    -not (Test-Path -LiteralPath $reviewSpecPath -PathType Leaf) -or
    -not (Test-Path -LiteralPath $registrySpecPath -PathType Leaf) -or
    -not (Test-Path -LiteralPath $accessibilitySpecPath -PathType Leaf)) {
  throw 'Web dependencies or owned admin browser tests are missing.'
}
if (-not (Test-Path -LiteralPath $fixtureDirectory -PathType Container)) {
  throw 'The exact WSL-home fixture directory is absent. Provision this run first.'
}

$env:LITECR_E2E_ADMIN_RUN_ID = $RunId
$env:LITECR_E2E_ADMIN_DATABASE_NAME = $databaseName
$env:LITECR_E2E_ADMIN_DISPOSABLE_DATABASE = 'true'
$env:LITECR_E2E_ADMIN_ALLOW_DATABASE_DROP = 'true'
$env:LITECR_E2E_ADMIN_FIXTURE_DIR = $fixtureDirectory
$env:LITECR_E2E_ADMIN_LOCALE = $Locale
$env:LITECR_E2E_REGISTRY_ID = "litecr-admin-e2e-$RunId"
$env:LITECR_E2E_ORGANIZATION_ID = "litecr-admin-org-$RunId"
$env:LITECR_E2E_TRANSLATOR_STORAGE_STATE = Join-Path $fixtureDirectory 'translator.json'
$env:LITECR_E2E_REVIEWER_STORAGE_STATE = Join-Path $fixtureDirectory 'reviewer.json'
$env:LITECR_E2E_PUBLISHER_STORAGE_STATE = Join-Path $fixtureDirectory 'publisher.json'
$env:LITECR_E2E_ADMINISTRATOR_STORAGE_STATE = Join-Path $fixtureDirectory 'administrator.json'
$env:LITECR_E2E_REGISTRAR_STORAGE_STATE = Join-Path $fixtureDirectory 'registrar.json'
$env:LITECR_E2E_STORAGE_STATE = $env:LITECR_E2E_REVIEWER_STORAGE_STATE
$env:LITECR_E2E_BASE_URL = $baseUrl
$env:LITECR_API_PROXY_TARGET = 'http://127.0.0.1:18080'

try {
  & wsl.exe -d Ubuntu -- bash /mnt/d/liteCR/apps/web/tests/fixtures/admin/check-admin-e2e-services.sh $RunId
  if ($LASTEXITCODE -ne 0) { throw 'Run-owned WSL API/Keycloak service verification failed.' }

  $apiReadyFromWindows = $false
  for ($attempt = 0; $attempt -lt 30; $attempt++) {
    try {
      $apiResponse = Invoke-WebRequest -Uri 'http://127.0.0.1:18080/health/ready' -TimeoutSec 2 -UseBasicParsing -ErrorAction Stop
      if ($apiResponse.StatusCode -eq 200) { $apiReadyFromWindows = $true; break }
    } catch { Start-Sleep -Seconds 1 }
  }
  if (-not $apiReadyFromWindows) {
    throw 'Windows could not reach the run-owned loopback API readiness endpoint; refusing to capture OIDC sessions or start browser tests.'
  }

  try {
    $identity = Invoke-RestMethod -Uri 'http://127.0.0.1:18081/realms/litecr/.well-known/openid-configuration' -TimeoutSec 5 -ErrorAction Stop
  } catch {
    $statusCode = 'none'
    if ($null -ne $_.Exception.Response -and $null -ne $_.Exception.Response.StatusCode) {
      $statusCode = [string]$_.Exception.Response.StatusCode.value__
    }
    throw "Windows could not fetch the Keycloak discovery endpoint (error_type=$($_.Exception.GetType().Name), status=$statusCode)."
  }
  if ($identity.issuer -cne 'http://127.0.0.1:18081/realms/litecr') {
    throw 'Windows reached a Keycloak discovery endpoint with an unexpected issuer.'
  }

  $listener = [System.Net.Sockets.TcpListener]::new([System.Net.IPAddress]::Loopback, 4173)
  try { $listener.Start() }
  catch { throw 'Port 4173 is occupied; refusing to reuse another process for the OIDC callback.' }
  finally { $listener.Stop() }

  New-Item -ItemType Directory -Path $runFolder -Force | Out-Null
  $previewConfig = Join-Path $runFolder 'vite.preview.config.mjs'
  @'
export default {
  preview: {
    proxy: {
      '/api': {
        target: process.env.LITECR_API_PROXY_TARGET ?? 'http://127.0.0.1:18080',
        changeOrigin: false,
      },
    },
  },
};
'@ | Set-Content -LiteralPath $previewConfig -Encoding utf8
  Push-Location $webRoot
  try {
    & $npm run build
    if ($LASTEXITCODE -ne 0) { throw 'The current web source did not build for the authenticated browser fixture.' }
  } finally { Pop-Location }
  $viteOut = Join-Path $runFolder 'vite.stdout.log'
  $viteErr = Join-Path $runFolder 'vite.stderr.log'
  $viteProcess = Start-Process -FilePath $node -ArgumentList @(
    $viteScript, 'preview', '--config', $previewConfig, '--host', '127.0.0.1', '--port', '4173', '--strictPort'
  ) -WorkingDirectory $webRoot -WindowStyle Hidden -PassThru `
    -RedirectStandardOutput $viteOut -RedirectStandardError $viteErr

  $webReady = $false
  for ($attempt = 0; $attempt -lt 30; $attempt++) {
    if ($viteProcess.HasExited) { throw 'The owned Vite server exited before becoming ready.' }
    try {
      $response = Invoke-WebRequest -Uri $baseUrl -TimeoutSec 2 -UseBasicParsing
      if ($response.StatusCode -eq 200) { $webReady = $true; break }
    } catch { Start-Sleep -Seconds 1 }
  }
  if (-not $webReady) { throw 'The owned Vite server did not become ready on loopback port 4173.' }

  $sessionOut = Join-Path $runFolder 'sessions.stdout.log'
  $sessionErr = Join-Path $runFolder 'sessions.stderr.log'
  $sessionProcess = Start-Process -FilePath $node -ArgumentList @($runnerScript) `
    -WorkingDirectory $webRoot -WindowStyle Hidden -PassThru `
    -RedirectStandardOutput $sessionOut -RedirectStandardError $sessionErr
  $sessionProcess.WaitForExit()
  if ($sessionProcess.ExitCode -ne 0) {
    $diagnostic = @((Get-SafeLogSummary $sessionOut), (Get-SafeLogSummary $sessionErr)) -join ' '
    throw "Capturing authenticated synthetic OIDC sessions failed. $diagnostic"
  }
  Get-Content -LiteralPath $sessionOut
  foreach ($role in @('translator', 'reviewer', 'publisher', 'administrator', 'registrar')) {
    if (-not (Test-Path -LiteralPath (Join-Path $fixtureDirectory "$role.json") -PathType Leaf)) {
      throw "The OIDC browser did not create the $role storage-state file."
    }
  }

  $seedOut = Join-Path $runFolder 'review-seed.stdout.log'
  $seedErr = Join-Path $runFolder 'review-seed.stderr.log'
  $seedProcess = Start-Process -FilePath $node -ArgumentList @($reviewSeedScript) `
    -WorkingDirectory $webRoot -WindowStyle Hidden -PassThru `
    -RedirectStandardOutput $seedOut -RedirectStandardError $seedErr
  $seedProcess.WaitForExit()
  if ($seedProcess.ExitCode -ne 0) {
    $diagnostic = @((Get-SafeLogSummary $seedOut), (Get-SafeLogSummary $seedErr)) -join ' '
    throw "Preparing the synthetic reviewer case failed. $diagnostic"
  }
  $reviewSeed = Get-Content -LiteralPath (Join-Path $fixtureDirectory 'review-case.json') -Raw | ConvertFrom-Json
  if ($reviewSeed.run_id -cne $RunId -or $reviewSeed.case_id -notmatch '^[a-f0-9-]{36}$' -or $reviewSeed.evidence_run_id -notmatch '^[a-f0-9-]{36}$' -or $reviewSeed.decision_case_id -notmatch '^[a-f0-9-]{36}$') {
    throw 'The reviewer seed artifact did not match this exact fixture run.'
  }
  $env:LITECR_E2E_REVIEW_CASE_ID = $reviewSeed.case_id
  $env:LITECR_E2E_REVIEW_EVIDENCE_RUN_ID = $reviewSeed.evidence_run_id
  $env:LITECR_E2E_REVIEW_DECISION_CASE_ID = $reviewSeed.decision_case_id

  $playwrightOut = Join-Path $runFolder 'playwright.stdout.log'
  $playwrightErr = Join-Path $runFolder 'playwright.stderr.log'
  $playwrightArgs = if ($ReviewerOnly) {
    @($playwrightCli, 'test', 'tests/e2e/review/reviewer-workspace.spec.ts', '--reporter=list', '--workers=1')
  } elseif ($RegistryOnly) {
    @($playwrightCli, 'test', 'tests/e2e/registry/identity-workflow.spec.ts', '--reporter=list', '--workers=1')
  } elseif ($FocusedUi) {
    @($playwrightCli, 'test', 'tests/e2e/accessibility/ui-quality.spec.ts', 'tests/e2e/admin/workflows.spec.ts:11', '--reporter=list', '--workers=1')
  } else {
    @($playwrightCli, 'test', 'tests/e2e/admin/workflows.spec.ts', 'tests/e2e/review/reviewer-workspace.spec.ts',
      'tests/e2e/accessibility/ui-quality.spec.ts', 'tests/e2e/registry/identity-workflow.spec.ts', '--reporter=list', '--workers=1')
  }
  $playwrightProcess = Start-Process -FilePath $node -ArgumentList $playwrightArgs -WorkingDirectory $webRoot -WindowStyle Hidden -PassThru `
    -RedirectStandardOutput $playwrightOut -RedirectStandardError $playwrightErr
  $playwrightProcess.WaitForExit()
  $testExit = $playwrightProcess.ExitCode
  if (Test-Path -LiteralPath $playwrightOut) { Get-Content -LiteralPath $playwrightOut }
  if (Test-Path -LiteralPath $playwrightErr) { Get-Content -LiteralPath $playwrightErr }
  if ($testExit -ne 0) { $cleanupErrors.Add("Focused Playwright exited with code $testExit.") }
  if ($null -ne $viteProcess) {
    $viteProcess.Refresh()
    if ($viteProcess.HasExited) {
      $diagnostic = @((Get-SafeLogSummary $viteOut), (Get-SafeLogSummary $viteErr)) -join ' '
      $cleanupErrors.Add("The runner-owned Vite server exited during or after the browser run (exit code $($viteProcess.ExitCode)). $diagnostic")
    }
  }
} catch {
  $cleanupErrors.Add($_.Exception.Message)
} finally {
  foreach ($managedProcess in @(
      @{ Process = $playwrightProcess; Script = $playwrightCli; Name = 'Playwright' },
      @{ Process = $seedProcess; Script = $reviewSeedScript; Name = 'review-case seed' },
      @{ Process = $sessionProcess; Script = $runnerScript; Name = 'OIDC browser capture' }
    )) {
    if ($null -eq $managedProcess.Process -or $managedProcess.Process.HasExited) { continue }
    try {
      $processInfo = Get-CimInstance Win32_Process -Filter "ProcessId = $($managedProcess.Process.Id)"
      if ($null -eq $processInfo -or $processInfo.ExecutablePath -ne $node -or
          $processInfo.CommandLine -notmatch [regex]::Escape($managedProcess.Script)) {
        $cleanupErrors.Add("The $($managedProcess.Name) process identity changed; it was left untouched for manual inspection.")
      } else {
        & taskkill.exe /PID $managedProcess.Process.Id /T /F | Out-Null
        if ($LASTEXITCODE -ne 0) { $cleanupErrors.Add("Could not stop the exact $($managedProcess.Name) process tree.") }
      }
    } catch { $cleanupErrors.Add("Could not stop the exact $($managedProcess.Name) process tree.") }
  }

  if ($null -ne $viteProcess) {
    try {
      $viteProcess.Refresh()
      if (-not $viteProcess.HasExited) {
        $processInfo = Get-CimInstance Win32_Process -Filter "ProcessId = $($viteProcess.Id)"
        $expectedCommand = [regex]::Escape($viteScript)
        if ($null -eq $processInfo -or $processInfo.ExecutablePath -ne $node -or
            $processInfo.CommandLine -notmatch $expectedCommand -or
            $processInfo.CommandLine -notmatch '--port 4173') {
          $cleanupErrors.Add('The Vite process identity changed; it was left untouched for manual inspection.')
        } else {
          Stop-Process -Id $viteProcess.Id -ErrorAction Stop
          Wait-Process -Id $viteProcess.Id -Timeout 10 -ErrorAction SilentlyContinue
        }
      }
    } catch { $cleanupErrors.Add('Could not stop the exact runner-owned Vite process.') }
  }

  & wsl.exe -d Ubuntu -- bash /mnt/d/liteCR/apps/web/tests/fixtures/admin/cleanup-admin-e2e.sh $RunId
  if ($LASTEXITCODE -ne 0) { $cleanupErrors.Add("WSL cleanup did not complete for run $RunId; its marker-owned directory was preserved.") }
}

if ($cleanupErrors.Count -gt 0) { throw ($cleanupErrors -join ' ') }
exit $testExit
