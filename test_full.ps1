# Full MCP Protocol Test
$proxyPath = "E:\AI-coding-creating\auggie-rs\target\release\mcp-proxy.exe"

Write-Host "=== Full MCP Protocol Test ===" -ForegroundColor Cyan

$psi = New-Object System.Diagnostics.ProcessStartInfo
$psi.FileName = $proxyPath
$psi.Arguments = '--node "C:\Program Files\nodejs\node.exe" --auggie-entry "C:\Users\marg0\AppData\Roaming\npm\node_modules\@augmentcode\auggie\augment.mjs" --default-root "E:\AI-coding-creating\agent\pochi" --log-level debug'
$psi.UseShellExecute = $false
$psi.RedirectStandardInput = $true
$psi.RedirectStandardOutput = $true
$psi.RedirectStandardError = $true
$psi.CreateNoWindow = $true

$process = [System.Diagnostics.Process]::Start($psi)
Start-Sleep -Seconds 2

# Test 1: Initialize
Write-Host "`n[1] Sending initialize..." -ForegroundColor Yellow
$init = '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2024-11-05","capabilities":{"roots":{"listChanged":true}},"clientInfo":{"name":"windsurf","version":"1.0"}}}'
$process.StandardInput.WriteLine($init)
$process.StandardInput.Flush()
Start-Sleep -Seconds 2

# Test 2: initialized notification
Write-Host "[2] Sending initialized notification..." -ForegroundColor Yellow
$initialized = '{"jsonrpc":"2.0","method":"notifications/initialized"}'
$process.StandardInput.WriteLine($initialized)
$process.StandardInput.Flush()
Start-Sleep -Seconds 1

# Test 3: List tools
Write-Host "[3] Sending tools/list..." -ForegroundColor Yellow
$listTools = '{"jsonrpc":"2.0","id":2,"method":"tools/list","params":{}}'
$process.StandardInput.WriteLine($listTools)
$process.StandardInput.Flush()
Start-Sleep -Seconds 3

Write-Host "`n=== Proxy stdout ===" -ForegroundColor Green
$output = ""
while ($process.StandardOutput.Peek() -ge 0) {
    $line = $process.StandardOutput.ReadLine()
    Write-Host $line
    $output += $line + "`n"
}

Write-Host "`n=== Proxy stderr (last 30 lines) ===" -ForegroundColor Red
$stderr = @()
while ($process.StandardError.Peek() -ge 0) {
    $stderr += $process.StandardError.ReadLine()
}
$stderr | Select-Object -Last 30 | ForEach-Object { Write-Host $_ }

$process.Kill()
Write-Host "`n=== Test Complete ===" -ForegroundColor Cyan
