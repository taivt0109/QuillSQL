# End-to-end test of the extension in SSMS, without a screen: run vsix\build.ps1 first, close SSMS,
# then .\vsix\e2e\run-e2e.ps1 -Version 22 (or 21). It installs the build into SSMS's experimental
# instance, creates the QuillTest database on (localdb)\MSSQLLocalDB, and drives the editor through
# the extension's test hook (a named pipe, open only when QUILLSQL_TEST_PIPE is set). Afterwards it
# puts your QuillSQL settings back, removes the extension from that instance and drops QuillTest.
param([ValidateSet('21', '22')][string]$Version = '22')
$ErrorActionPreference = 'Stop'
$here = $PSScriptRoot
$repo = Split-Path (Split-Path $here -Parent) -Parent
$release = "$env:ProgramFiles\Microsoft SQL Server Management Studio $Version\Release"
if (Get-Process SSMS -ErrorAction SilentlyContinue) { throw 'Close SSMS first.' }

$work = Join-Path $env:TEMP 'quillsql-e2e'
New-Item -ItemType Directory -Force $work | Out-Null
$csc = Get-ChildItem "$env:ProgramFiles\Microsoft Visual Studio\*\*\MSBuild\Current\Bin\Roslyn\csc.exe" -ErrorAction SilentlyContinue | Select-Object -First 1
if (-not $csc) { throw 'csc.exe (Visual Studio) was not found.' }
& $csc.FullName /nologo /out:"$work\HookDriver.exe" /r:System.Core.dll "$here\HookDriver.cs"
if ($LASTEXITCODE -ne 0) { throw 'The driver did not compile.' }

$q = "$env:LOCALAPPDATA\QuillSQL"
New-Item -ItemType Directory -Force $q | Out-Null
$logPath = "$q\quillsql.log"
$logStart = if (Test-Path $logPath) { (Get-Item $logPath).Length } else { 0 }
$saved = @{}
foreach ($f in 'settings.json', 'picks.txt') {
    if (Test-Path "$q\$f") { Copy-Item "$q\$f" "$work\$f.backup" -Force; $saved[$f] = $true }
}
$ErrorActionPreference = 'Continue'
try {
    sqlcmd -S '(localdb)\MSSQLLocalDB' -i "$here\QuillTest.sql" -b | Out-Null
    & "$repo\vsix\install.ps1" -SsmsPath $release -RootSuffix Exp | Out-Null
    # The instance can keep pointing at a removed install folder; rebuild its registrations.
    $u = Start-Process "$release\Common7\IDE\SSMS.exe" -ArgumentList '/rootsuffix', 'Exp', '/updateconfiguration' -PassThru
    if (-not $u.WaitForExit(300000)) { Stop-Process -Id $u.Id -Force }
    $driver = Start-Process "$work\HookDriver.exe" -ArgumentList "`"$release\Common7\IDE`"" -RedirectStandardOutput "$work\result.txt" -PassThru -NoNewWindow
    if (-not $driver.WaitForExit(900000)) { 'The driver timed out.'; Stop-Process -Id $driver.Id -Force }
} finally {
    foreach ($f in 'settings.json', 'picks.txt') {
        if ($saved[$f]) { Copy-Item "$work\$f.backup" "$q\$f" -Force } else { Remove-Item "$q\$f" -ErrorAction SilentlyContinue }
    }
    Get-CimInstance Win32_Process -Filter "Name='SSMS.exe'" | Where-Object { $_.CommandLine -match 'rootsuffix Exp' } | ForEach-Object { Stop-Process -Id $_.ProcessId -Force }
    try { & "$repo\vsix\install.ps1" -SsmsPath $release -RootSuffix Exp -Uninstall | Out-Null } catch { "uninstall: $_" }
    sqlcmd -S '(localdb)\MSSQLLocalDB' -Q "IF DB_ID('QuillTest') IS NOT NULL BEGIN ALTER DATABASE QuillTest SET SINGLE_USER WITH ROLLBACK IMMEDIATE; DROP DATABASE QuillTest; END" | Out-Null
}
Get-Content "$work\result.txt" -Encoding UTF8
# Errors the extension logged during the run fail it too (a card that could not be drawn, ...).
if (Test-Path $logPath) {
    $fs = [IO.File]::Open($logPath, 'Open', 'Read', 'ReadWrite'); $fs.Seek($logStart, 'Begin') | Out-Null
    $errors = (New-Object IO.StreamReader($fs)).ReadToEnd() -split "`n" | Where-Object { $_ -match ' ERROR ' }; $fs.Close()
    if ($errors) { 'FAIL the extension logged errors:'; $errors | Select-Object -First 10 | ForEach-Object { '   ' + $_.Trim() } } else { 'PASS no errors in the log' }
}
