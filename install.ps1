# Installs or updates QuillSQL from its latest GitHub release:
#
#   irm https://raw.githubusercontent.com/taivt0109/QuillSQL/main/install.ps1 | iex
#
# It installs the SSMS extension into every SSMS 21 or later on this machine, and the `quill`
# command line tool into %LOCALAPPDATA%\QuillSQL\bin (added to your PATH). Nothing needs admin.
# Options, as environment variables set before running it:
#   $env:QUILLSQL_VERSION   = '0.3.0'   install this version instead of the latest
#   $env:QUILLSQL_NO_CLI    = '1'       skip the command line tool
#   $env:QUILLSQL_UNINSTALL = '1'       remove QuillSQL instead
#   $env:QUILLSQL_ROOT_SUFFIX = 'Exp'   use an SSMS instance started with /rootsuffix
#   $env:QUILLSQL_UPDATE_URL = 'https://...'  read the release from a mirror of the GitHub API

function Install-QuillSql {
    $ErrorActionPreference = 'Stop'
    $ProgressPreference = 'SilentlyContinue'
    [Net.ServicePointManager]::SecurityProtocol = [Net.ServicePointManager]::SecurityProtocol -bor [Net.SecurityProtocolType]::Tls12
    $repo = 'taivt0109/QuillSQL'
    $extensionId = 'QuillSql.Ssms.5f0c7a52-3b1e-4d7c-9a46-6e2d1c8b7f10'
    $data = Join-Path $env:LOCALAPPDATA 'QuillSQL'
    $bin = Join-Path $data 'bin'

    # SSMS 21 and later register with the Visual Studio installer; older SSMS is not supported.
    $ssms = @()
    $vswhere = "${env:ProgramFiles(x86)}\Microsoft Visual Studio\Installer\vswhere.exe"
    if (Test-Path $vswhere) {
        # Parenthesized so Windows PowerShell 5.1 passes the instances one by one, not as one array.
        $instances = (& $vswhere -all -prerelease -products * -format json | Out-String | ConvertFrom-Json)
        $ssms += @($instances) | Where-Object { $_.productId -like '*Ssms*' } | ForEach-Object { $_.installationPath }
    }
    $default = "$env:ProgramFiles\Microsoft SQL Server Management Studio 21\Release"
    if (Test-Path $default) { $ssms += $default }
    $ssms = $ssms | Where-Object { Test-Path (Join-Path $_ 'Common7\IDE\VSIXInstaller.exe') } | Sort-Object -Unique
    if (-not $ssms) { throw 'SSMS 21 or later was not found. Install it from https://aka.ms/ssms first.' }

    $suffix = $env:QUILLSQL_ROOT_SUFFIX
    $suffixArgument = @(if ($suffix) { "/rootSuffix:$suffix" })
    # Only the SSMS instance being changed has to be closed.
    function Get-Running {
        Get-CimInstance Win32_Process -Filter "Name = 'SSMS.exe'" | Where-Object {
            $command = [string]$_.CommandLine
            if ($suffix) { $command -match "/rootsuffix\s+$([regex]::Escape($suffix))\b" } else { $command -notmatch '/rootsuffix' }
        }
    }
    while (Get-Running) {
        Write-Host 'Close SQL Server Management Studio, then press Enter (Ctrl+C to stop).' -ForegroundColor Yellow
        [void](Read-Host)
    }

    function Clear-EditorCache {
        # SSMS caches its editor components and may not notice a changed extension.
        Get-ChildItem (Join-Path $env:LOCALAPPDATA 'Microsoft\SSMS') -Directory -ErrorAction SilentlyContinue |
            Where-Object { $_.Name -match ('^\d+\.\d+_[0-9a-f]+' + [regex]::Escape([string]$suffix) + '$') } |
            ForEach-Object { Remove-Item (Join-Path $_.FullName 'ComponentModelCache') -Recurse -Force -ErrorAction SilentlyContinue }
    }

    if ($env:QUILLSQL_UNINSTALL) {
        foreach ($path in $ssms) {
            $p = Start-Process (Join-Path $path 'Common7\IDE\VSIXInstaller.exe') -ArgumentList (@('/quiet') + $suffixArgument + "/uninstall:$extensionId") -Wait -PassThru
            Write-Host "SSMS at ${path}: uninstall exit code $($p.ExitCode)"
        }
        Clear-EditorCache
        Remove-Item $bin -Recurse -Force -ErrorAction SilentlyContinue
        $path = [Environment]::GetEnvironmentVariable('Path', 'User')
        if ($path) {
            [Environment]::SetEnvironmentVariable('Path', (($path -split ';') | Where-Object { $_ -and $_ -ne $bin }) -join ';', 'User')
        }
        Write-Host 'QuillSQL removed. Your settings and history in' $data 'were kept.' -ForegroundColor Green
        return
    }

    $api = "https://api.github.com/repos/$repo/releases/" + $(if ($env:QUILLSQL_VERSION) { "tags/v$($env:QUILLSQL_VERSION.TrimStart('v'))" } else { 'latest' })
    if ($env:QUILLSQL_UPDATE_URL) { $api = $env:QUILLSQL_UPDATE_URL }
    $release = Invoke-RestMethod $api -Headers @{ 'User-Agent' = 'QuillSQL-installer'; 'Accept' = 'application/vnd.github+json' }
    Write-Host "QuillSQL $($release.tag_name)" -ForegroundColor Cyan

    $download = Join-Path ([IO.Path]::GetTempPath()) ("quillsql-" + $release.tag_name)
    New-Item -ItemType Directory -Force $download | Out-Null
    function Get-Asset([string]$name) {
        $asset = $release.assets | Where-Object { $_.name -eq $name } | Select-Object -First 1
        if (-not $asset) { throw "The release has no $name." }
        $file = Join-Path $download $name
        Invoke-WebRequest $asset.browser_download_url -OutFile $file -UseBasicParsing -Headers @{ 'User-Agent' = 'QuillSQL-installer' }
        return $file
    }
    $sums = @{}
    foreach ($line in Get-Content (Get-Asset 'SHA256SUMS.txt')) {
        $parts = $line.Trim() -split '\s+\*?', 2
        if ($parts.Count -eq 2) { $sums[$parts[1]] = $parts[0].ToLowerInvariant() }
    }
    function Get-Verified([string]$name) {
        $file = Get-Asset $name
        $hash = (Get-FileHash $file -Algorithm SHA256).Hash.ToLowerInvariant()
        if ($sums[$name] -ne $hash) { throw "$name does not match SHA256SUMS.txt; nothing was installed." }
        return $file
    }

    $vsix = Get-Verified 'QuillSQL.vsix'
    foreach ($path in $ssms) {
        $p = Start-Process (Join-Path $path 'Common7\IDE\VSIXInstaller.exe') -ArgumentList (@('/quiet') + $suffixArgument + "`"$vsix`"") -Wait -PassThru
        # 1001: this version is already installed.
        if ($p.ExitCode -eq 1001) { Write-Host "SSMS at $path already has this version"; continue }
        if ($p.ExitCode -ne 0) { throw "VSIXInstaller failed for $path with exit code $($p.ExitCode); see %TEMP%\dd_VSIXInstaller_*.log." }
        Write-Host "Installed the extension into SSMS at $path"
    }
    Clear-EditorCache

    if (-not $env:QUILLSQL_NO_CLI) {
        $exe = Get-Verified 'quill.exe'
        New-Item -ItemType Directory -Force $bin | Out-Null
        Copy-Item $exe (Join-Path $bin 'quill.exe') -Force
        $path = [Environment]::GetEnvironmentVariable('Path', 'User')
        if (-not (($path -split ';') -contains $bin)) {
            [Environment]::SetEnvironmentVariable('Path', ((@($path, $bin) | Where-Object { $_ }) -join ';'), 'User')
            Write-Host "Added $bin to your PATH; open a new terminal to use quill."
        }
        Write-Host "Installed the quill command line tool to $bin"
    }
    Remove-Item $download -Recurse -Force -ErrorAction SilentlyContinue
    Write-Host 'Done. Start SSMS; QuillSQL is under Tools > QuillSQL. It checks for updates once a day.' -ForegroundColor Green
}

Install-QuillSql
