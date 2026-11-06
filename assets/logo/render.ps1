# Renders the logo SVGs to PNG with headless Chrome or Edge, then makes the smaller sizes and
# quillsql.ico with Python's Pillow. Run from any folder: .\assets\logo\render.ps1
$ErrorActionPreference = 'Stop'
$here = $PSScriptRoot
$browser = @(
    "$env:ProgramFiles\Google\Chrome\Application\chrome.exe",
    "${env:ProgramFiles(x86)}\Microsoft\Edge\Application\msedge.exe"
) | Where-Object { Test-Path $_ } | Select-Object -First 1
if (-not $browser) { throw 'Chrome or Edge is needed to render the SVGs.' }

$profile = Join-Path ([IO.Path]::GetTempPath()) 'quillsql-logo-render'
New-Item -ItemType Directory -Force $profile | Out-Null
$png = Join-Path $here 'png'
New-Item -ItemType Directory -Force $png | Out-Null

function Render([string]$svg, [int]$width, [int]$height, [string]$out) {
    # Scale by rewriting the root size; the viewBox keeps the drawing the same.
    $text = Get-Content (Join-Path $here $svg) -Raw
    $text = [regex]::Replace($text, '^(<svg[^>]*?) width="\d+" height="\d+"', "`$1 width=`"$width`" height=`"$height`"")
    $file = Join-Path $profile ("{0}-{1}.svg" -f [IO.Path]::GetFileNameWithoutExtension($svg), $width)
    [IO.File]::WriteAllText($file, $text)
    $shot = Join-Path $profile 'shot.png'
    Remove-Item $shot -ErrorAction SilentlyContinue
    $url = 'file:///' + ($file -replace '\\', '/' -replace ' ', '%20')
    $p = Start-Process $browser -Wait -PassThru -ArgumentList '--headless=new', '--disable-gpu', '--hide-scrollbars', '--no-first-run',
        "`"--user-data-dir=$profile`"", '--default-background-color=00000000', "`"--screenshot=$shot`"", "--window-size=$width,$height", $url
    if ($p.ExitCode -ne 0 -or -not (Test-Path $shot)) { throw "Rendering $svg failed." }
    Move-Item $shot $out -Force
}

$master = Join-Path $profile 'icon-1024.png'
$small = Join-Path $profile 'icon-small-1024.png'
Render 'quillsql-icon.svg' 1024 1024 $master
Render 'quillsql-icon-small.svg' 1024 1024 $small
Render 'quillsql-logo.svg' 1280 320 (Join-Path $png 'quillsql-logo.png')
Render 'quillsql-logo-dark.svg' 1280 320 (Join-Path $png 'quillsql-logo-dark.png')

python -c @"
from PIL import Image
import os, sys
master, small, png, ico = sys.argv[1:5]
big = Image.open(master).convert('RGBA')
simple = Image.open(small).convert('RGBA')
# Up to 32 pixels the simplified mark is used; larger sizes use the full one.
def at(size):
    return (simple if size <= 32 else big).resize((size, size), Image.LANCZOS)
for size in (16, 24, 32, 48, 64, 90, 128, 200, 256, 512):
    at(size).save(os.path.join(png, 'quillsql-%d.png' % size))
sizes = (16, 24, 32, 48, 64, 128, 256)
at(256).save(ico, format='ICO', sizes=[(s, s) for s in sizes], append_images=[at(s) for s in sizes[:-1]])
"@ $master $small $png (Join-Path $here 'quillsql.ico')
if ($LASTEXITCODE -ne 0) { throw 'Resizing or building the .ico failed.' }
Write-Host "Rendered to $png and $(Join-Path $here 'quillsql.ico')"
