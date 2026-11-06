# Installs the built VSIX into SSMS. Close SSMS first.
#   -RootSuffix Exp   install into an isolated experimental instance instead
#   -Uninstall        remove the extension
param(
    [string]$SsmsPath = "$env:ProgramFiles\Microsoft SQL Server Management Studio 21\Release",
    [string]$RootSuffix,
    [switch]$Uninstall
)
$ErrorActionPreference = 'Stop'

$installer = Join-Path $SsmsPath 'Common7\IDE\VSIXInstaller.exe'
if (-not (Test-Path $installer)) { throw "VSIXInstaller.exe was not found under $SsmsPath." }

$arguments = @('/quiet')
if ($RootSuffix) { $arguments += "/rootSuffix:$RootSuffix" }
if ($Uninstall) {
    $arguments += '/uninstall:QuillSql.Ssms.5f0c7a52-3b1e-4d7c-9a46-6e2d1c8b7f10'
} else {
    $vsix = Join-Path $PSScriptRoot 'QuillSql.Vsix\bin\Release\QuillSql.Vsix.vsix'
    if (-not (Test-Path $vsix)) { throw "Run build.ps1 first: $vsix does not exist." }
    $arguments += "`"$vsix`""
}

$process = Start-Process -FilePath $installer -ArgumentList $arguments -Wait -PassThru
if ($process.ExitCode -ne 0) { throw "VSIXInstaller failed with exit code $($process.ExitCode). See %TEMP%\dd_VSIXInstaller_*.log." }

# SSMS caches its editor components and does not always notice a reinstalled build, so clear
# the cache of this instance only; SSMS rebuilds it on the next start.
$instancePattern = '^\d+\.\d+_[0-9a-f]+' + [regex]::Escape($RootSuffix) + '$'
Get-ChildItem (Join-Path $env:LOCALAPPDATA 'Microsoft\SSMS') -Directory -ErrorAction SilentlyContinue |
    Where-Object { $_.Name -match $instancePattern } |
    ForEach-Object { Remove-Item (Join-Path $_.FullName 'ComponentModelCache') -Recurse -Force -ErrorAction SilentlyContinue }
Write-Host 'Done. Start SSMS; the commands are under Tools > QuillSQL.'
