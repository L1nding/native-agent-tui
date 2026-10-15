[CmdletBinding()]
param(
    [string]$ModelerRepo = (Join-Path $PSScriptRoot '../../blender-agent-modeler'),
    [string]$Project = 'projects/hospital-site',
    [string]$Model,
    [string]$Binary = (Join-Path $PSScriptRoot '../target/workflow-final/release/native-agent-tui.exe')
)
$ErrorActionPreference = 'Stop'
$taskRepository = (Resolve-Path -LiteralPath $ModelerRepo).Path
$taskProject = [IO.Path]::GetFullPath((Join-Path $taskRepository $Project))
if (-not (Test-Path -LiteralPath (Join-Path $taskProject 'project.yaml') -PathType Leaf)) {
    throw "Project '$Project' has no project.yaml. Pass -Project projects/<project-name>."
}
if (-not (Test-Path -LiteralPath $Binary -PathType Leaf)) {
    if ($PSBoundParameters.ContainsKey('Binary')) { throw "TUI binary does not exist: $Binary" }
    Push-Location (Join-Path $PSScriptRoot '..')
    try {
        & cargo build --locked --release --bin native-agent-tui --target-dir target/workflow-final
        if ($LASTEXITCODE -ne 0) { throw 'TUI build failed; see compiler output above.' }
    } finally { Pop-Location }
}
$taskBinary = (Resolve-Path -LiteralPath $Binary).Path
$taskArguments = @('--cwd', $taskRepository, '--domain-project', $Project, '--permission', 'read-only', '--tui')
if ($Model) { $taskArguments += @('--model', $Model) }
Clear-Host
& $taskBinary @taskArguments
exit $LASTEXITCODE
