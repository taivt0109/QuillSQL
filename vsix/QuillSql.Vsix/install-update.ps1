param([int]$WaitFor, [string]$Ide, [string]$Installer, [string]$Vsix, [string]$Version, [string]$RootSuffix, [string]$Cache, [string]$Log, [string]$Result, [string]$Icon)
# Installs a QuillSQL update once SSMS has closed, in a small window that says what it is doing,
# so nobody opens SSMS halfway through, and that says how it ended.
Add-Type -AssemblyName System.Windows.Forms, System.Drawing
# Drawn at the screen's own resolution: without this Windows stretches the window as a bitmap
# on a high-DPI screen and the text is blurred.
Add-Type -Namespace QuillSql -Name Dpi -MemberDefinition @'
[DllImport("user32.dll")] public static extern bool SetProcessDpiAwarenessContext(IntPtr value);
[DllImport("user32.dll")] public static extern bool SetProcessDPIAware();
'@
try { if (-not [QuillSql.Dpi]::SetProcessDpiAwarenessContext([IntPtr]-4)) { [void][QuillSql.Dpi]::SetProcessDPIAware() } } catch { }
[System.Windows.Forms.Application]::EnableVisualStyles()
$form = New-Object System.Windows.Forms.Form
$form.Text = "QuillSQL $Version"
if ($Icon -and (Test-Path -LiteralPath $Icon)) { $form.Icon = New-Object System.Drawing.Icon($Icon) }
$form.Font = New-Object System.Drawing.Font('Segoe UI', 9)
$form.TopMost = $true
$form.FormBorderStyle = 'FixedDialog'; $form.MaximizeBox = $false; $form.MinimizeBox = $false
$form.StartPosition = 'CenterScreen'
# The window grows to fit its text at any DPI: fonts are in points, other sizes are scaled here.
$form.AutoScaleMode = 'None'
$form.AutoSize = $true; $form.AutoSizeMode = 'GrowAndShrink'
$g = $form.CreateGraphics(); $scale = $g.DpiX / 96; $g.Dispose()
function Px([int]$n) { [int][Math]::Round($n * $scale) }
$layout = New-Object System.Windows.Forms.TableLayoutPanel
$layout.AutoSize = $true; $layout.AutoSizeMode = 'GrowAndShrink'; $layout.ColumnCount = 1
# The margin around the content belongs to the layout: an auto-sized form ignores its own padding.
$layout.Padding = New-Object System.Windows.Forms.Padding((Px 20), (Px 18), (Px 20), (Px 16))
$label = New-Object System.Windows.Forms.Label
$label.AutoSize = $true
$label.MaximumSize = New-Object System.Drawing.Size((Px 460), 0)
$label.MinimumSize = New-Object System.Drawing.Size((Px 460), 0)
$label.Font = New-Object System.Drawing.Font('Segoe UI', 10)
$label.Margin = New-Object System.Windows.Forms.Padding(0, 0, 0, (Px 12))
$bar = New-Object System.Windows.Forms.ProgressBar
$bar.Width = Px 460; $bar.Height = Px 14; $bar.Style = 'Marquee'
$bar.Margin = New-Object System.Windows.Forms.Padding(0, 0, 0, (Px 12))
$buttons = New-Object System.Windows.Forms.FlowLayoutPanel
$buttons.FlowDirection = 'RightToLeft'; $buttons.AutoSize = $true; $buttons.AutoSizeMode = 'GrowAndShrink'
$buttons.Dock = 'Right'; $buttons.Margin = New-Object System.Windows.Forms.Padding(0)
function New-Button([string]$text) {
    $b = New-Object System.Windows.Forms.Button
    $b.Text = $text; $b.AutoSize = $true; $b.Visible = $false
    $b.MinimumSize = New-Object System.Drawing.Size((Px 96), (Px 28))
    $b.Padding = New-Object System.Windows.Forms.Padding((Px 8), 0, (Px 8), 0)
    $b.Margin = New-Object System.Windows.Forms.Padding((Px 8), 0, 0, 0)
    $b
}
$close = New-Button '{{CLOSE}}'
$retry = New-Button '{{RETRY}}'
$openLog = New-Button '{{OPEN_LOG}}'
$buttons.Controls.AddRange(@($close, $retry, $openLog))
$script:choice = $null
$close.Add_Click({ $script:choice = 'close' })
$retry.Add_Click({ $script:choice = 'retry' })
$openLog.Add_Click({ Start-Process notepad.exe -ArgumentList ('"' + $vsixLog + '"') })
$form.Add_FormClosing({ if (-not $script:choice) { $script:choice = 'close' } })
$layout.Controls.Add($label, 0, 0)
$layout.Controls.Add($bar, 0, 1)
$layout.Controls.Add($buttons, 0, 2)
$form.Controls.Add($layout)
$form.Show()

function Pump([double]$seconds) {
    $end = (Get-Date).AddSeconds($seconds)
    do { [System.Windows.Forms.Application]::DoEvents(); Start-Sleep -Milliseconds 100 } while ((Get-Date) -lt $end)
}
function Say([string]$text) { $label.Text = $text; [System.Windows.Forms.Application]::DoEvents() }
function Write-Log([string]$text) { Add-Content -LiteralPath $Log -Value ((Get-Date -Format 'yyyy-MM-dd HH:mm:ss.fff') + ' ' + $text) }
# Every SSMS of this installation, not only the one that scheduled the update.
function Get-Ssms { @(Get-Process SSMS -ErrorAction SilentlyContinue | Where-Object { $_.Path -and $_.Path.StartsWith($Ide, [StringComparison]::OrdinalIgnoreCase) }) }

$vsixLog = Join-Path $env:TEMP 'quillsql-update.log'
$arguments = @('/quiet', '/logFile:quillsql-update.log')
if ($RootSuffix) { $arguments += "/rootSuffix:$RootSuffix" }
$arguments += ('"' + $Vsix + '"')
while ($true) {
    $script:choice = $null
    $bar.Visible = $true; $close.Visible = $false; $retry.Visible = $false; $openLog.Visible = $false
    while ((Get-Ssms).Count -gt 0) {
        Say '{{WAITING}}'
        Pump 1
        if ($script:choice -eq 'close') { Write-Log "update $Version cancelled while SSMS was open"; exit }
    }
    Say '{{INSTALLING}}'
    $p = Start-Process -FilePath $Installer -ArgumentList $arguments -PassThru
    while (-not $p.HasExited) { Pump 0.5 }
    $code = $p.ExitCode
    $bar.Visible = $false
    if ($code -eq 0 -or $code -eq 1001) {
        if ($Cache) { Remove-Item -LiteralPath $Cache -Recurse -Force -ErrorAction SilentlyContinue }
        Set-Content -LiteralPath $Result -Value "installed|$Version" -Encoding UTF8
        Write-Log "update $Version installed from $Vsix"
        Say '{{DONE}}'
        $close.Visible = $true
        # Closes by itself after a while.
        $end = (Get-Date).AddSeconds(20)
        while (-not $script:choice -and (Get-Date) -lt $end) { Pump 0.3 }
        break
    }
    $reopened = (Get-Ssms).Count -gt 0
    Set-Content -LiteralPath $Result -Value "failed|$Version|$code" -Encoding UTF8
    Write-Log "update $Version failed: VSIXInstaller exit code $code$(if ($reopened) { ', SSMS was opened during the install' }); see $vsixLog"
    if ($reopened) { Say '{{REOPENED}}' } else { Say ('{{FAILED}}' + " ($code)") }
    $close.Visible = $true; $retry.Visible = $true; $openLog.Visible = $true
    while (-not $script:choice) { Pump 0.3 }
    if ($script:choice -ne 'retry') { break }
}
$form.Close()
