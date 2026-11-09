param([int]$WaitFor, [string]$Ide, [string]$Installer, [string]$Vsix, [string]$Version, [string]$RootSuffix, [string]$Cache, [string]$Log, [string]$Result)
# Installs a QuillSQL update once SSMS has closed, in a small window that says what it is doing,
# so nobody opens SSMS halfway through, and that says how it ended.
Add-Type -AssemblyName System.Windows.Forms, System.Drawing
[System.Windows.Forms.Application]::EnableVisualStyles()
$form = New-Object System.Windows.Forms.Form
$form.Text = "QuillSQL $Version"
$form.Width = 520; $form.Height = 190
$form.TopMost = $true
$form.FormBorderStyle = 'FixedDialog'; $form.MaximizeBox = $false; $form.MinimizeBox = $false
$form.StartPosition = 'CenterScreen'
$label = New-Object System.Windows.Forms.Label
$label.Left = 18; $label.Top = 16; $label.Width = 470; $label.Height = 60
$label.Font = New-Object System.Drawing.Font('Segoe UI', 10)
$bar = New-Object System.Windows.Forms.ProgressBar
$bar.Left = 20; $bar.Top = 80; $bar.Width = 466; $bar.Height = 14; $bar.Style = 'Marquee'
function New-Button([string]$text, [int]$left) {
    $b = New-Object System.Windows.Forms.Button
    $b.Text = $text; $b.Left = $left; $b.Top = 108; $b.Width = 110; $b.Height = 28; $b.Visible = $false
    $b
}
$close = New-Button '{{CLOSE}}' 376
$retry = New-Button '{{RETRY}}' 256
$openLog = New-Button '{{OPEN_LOG}}' 136
$script:choice = $null
$close.Add_Click({ $script:choice = 'close' })
$retry.Add_Click({ $script:choice = 'retry' })
$openLog.Add_Click({ Start-Process notepad.exe -ArgumentList ('"' + $vsixLog + '"') })
$form.Add_FormClosing({ if (-not $script:choice) { $script:choice = 'close' } })
$form.Controls.AddRange(@($label, $bar, $close, $retry, $openLog))
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
