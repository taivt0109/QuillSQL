# Sets the version everywhere it is written: .\scripts\set-version.ps1 0.3.0
# Then commit, tag v0.3.0 and push the tag; the Release workflow publishes it.
param([Parameter(Mandatory)][ValidatePattern('^\d+\.\d+\.\d+$')][string]$Version)
$ErrorActionPreference = 'Stop'
$root = Split-Path $PSScriptRoot -Parent

function Update([string]$file, [string]$pattern, [string]$replacement) {
    $path = Join-Path $root $file
    $text = [IO.File]::ReadAllText($path)
    $updated = [regex]::Replace($text, $pattern, $replacement, 'Multiline')
    if ($updated -eq $text -and $text -notmatch [regex]::Escape($Version)) { throw "No version found in $file." }
    [IO.File]::WriteAllText($path, $updated)
    Write-Host "Updated $file"
}

Update 'Cargo.toml' '^version = "[^"]+"' "version = `"$Version`""
Update 'vsix\QuillSql.Vsix\source.extension.vsixmanifest' '(<Identity [^>]*Version=")[^"]+' "`${1}$Version"
Update 'vsix\QuillSql.Vsix\QuillSqlPackage.cs' 'const string Version = "[^"]+"' "const string Version = `"$Version`""
Write-Host "Version $Version. Commit, then: git tag v$Version; git push origin v$Version"
