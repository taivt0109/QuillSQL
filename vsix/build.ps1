# Builds the Rust engine and packages it into bin\Release\QuillSql.Vsix.vsix.
$ErrorActionPreference = 'Stop'

$vswhere = "${env:ProgramFiles(x86)}\Microsoft Visual Studio\Installer\vswhere.exe"
$msbuild = & $vswhere -latest -products * -requires Microsoft.Component.MSBuild -find 'MSBuild\Current\Bin\MSBuild.exe' | Select-Object -First 1
if (-not $msbuild) { throw 'MSBuild from Visual Studio 2022 was not found.' }

& $msbuild "$PSScriptRoot\QuillSql.Vsix\QuillSql.Vsix.csproj" /restore /t:Rebuild /p:Configuration=Release /v:minimal /nologo
if ($LASTEXITCODE -ne 0) { throw "MSBuild failed with exit code $LASTEXITCODE." }

Write-Host "Built $PSScriptRoot\QuillSql.Vsix\bin\Release\QuillSql.Vsix.vsix"
