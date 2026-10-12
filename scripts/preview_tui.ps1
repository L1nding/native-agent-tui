param(
    [ValidateSet('running', 'empty', 'approval', 'failed', 'unknown', 'commands', 'request-panel', 'input', 'help', 'tools', 'timeline', 'search', 'evidence', 'workflow', 'context', 'skills')]
    [string]$Scene = 'running',
    [ValidateSet('dark', 'light', 'terminal', 'mono')]
    [string]$Theme = 'dark',
    [ValidateSet('zh', 'en')]
    [string]$Lang = 'zh',
    [ValidateRange(24, 240)][int]$Columns = 120,
    [ValidateRange(8, 80)][int]$Rows = 36
)

# A development-only preview with synthetic data and no execution transport.
$ErrorActionPreference = 'Stop'
$repoRoot = Split-Path -Parent $PSScriptRoot
Push-Location -LiteralPath $repoRoot
try {
    $messages = & cargo test --locked --lib --no-run --message-format=json
    if ($LASTEXITCODE -ne 0) { throw 'Preview build failed.' }
    $artifacts = $messages | ForEach-Object { $_ | ConvertFrom-Json }
    $binary = $artifacts | Where-Object { $_.reason -eq 'compiler-artifact' -and $_.executable -and $_.profile.test } | Select-Object -Last 1
    if (-not $binary) { throw 'Cargo did not return a test executable.' }
    $previewDir = Join-Path $repoRoot 'target/ui-previews'
    [void](New-Item -ItemType Directory -Path $previewDir -Force)
    $previewId = [guid]::NewGuid().ToString('N')
    $copyPath = Join-Path $previewDir "preview-$previewId.exe"
    $launchFile = Join-Path $previewDir "launch-$previewId.ps1"
    $controlFile = Join-Path $previewDir "control-$previewId.txt"
    Copy-Item -LiteralPath $binary.executable -Destination $copyPath
    $script = @"
`$env:NO_COLOR=''
`$env:TUI_INTERACTIVE_PREVIEW='1'
`$env:NATIVE_AGENT_TUI_THEME='$Theme'
`$env:TUI_PREVIEW_LANG='$Lang'
`$env:TUI_PREVIEW_SCENE='$Scene'
`$env:TUI_PREVIEW_CONTROL='$($controlFile.Replace("'", "''"))'
Set-Location -LiteralPath '$($repoRoot.Replace("'", "''"))'
& '$($copyPath.Replace("'", "''"))' 'ui::visual_tests::interactive_visual_preview' --ignored --nocapture --test-threads=1
"@
    Set-Content -LiteralPath $launchFile -Value $script -Encoding utf8
    Start-Process -FilePath 'wt.exe' -ArgumentList "-w new --size $Columns,$Rows new-tab --title `"Native Agent Preview - $Theme - $Scene`" powershell.exe -NoLogo -NoProfile -File `"$launchFile`""
    [pscustomobject]@{ theme=$Theme; scene=$Scene; controlFile=$controlFile; executable=$copyPath } | ConvertTo-Json -Compress
}
finally { Pop-Location }
