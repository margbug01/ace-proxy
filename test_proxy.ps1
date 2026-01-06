# Test MCP Proxy
$proxyPath = "E:\AI-coding-creating\auggie-rs\target\release\mcp-proxy.exe"
$initRequest = '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2024-11-05","capabilities":{},"clientInfo":{"name":"test","version":"1.0"}}}'

Write-Host "Testing MCP Proxy..." -ForegroundColor Cyan

# Start proxy process
$psi = New-Object System.Diagnostics.ProcessStartInfo
$psi.FileName = $proxyPath
$psi.Arguments = '--node "C:\Program Files\nodejs\node.exe" --auggie-entry "C:\Users\marg0\AppData\Roaming\npm\node_modules\@augmentcode\auggie\augment.mjs" --default-root "E:\AI-coding-creating\agent\pochi" --log-level debug'
$psi.UseShellExecute = $false
$psi.RedirectStandardInput = $true
$psi.RedirectStandardOutput = $true
$psi.RedirectStandardError = $true
$psi.CreateNoWindow = $true

$process = [System.Diagnostics.Process]::Start($psi)

# Wait for startup
Start-Sleep -Seconds 2

Write-Host "`nSending initialize request:" -ForegroundColor Yellow
Write-Host $initRequest

# Send initialize request
$process.StandardInput.WriteLine($initRequest)
$process.StandardInput.Flush()

# Wait for response
Start-Sleep -Seconds 3

Write-Host "`nProxy stdout:" -ForegroundColor Green
while ($process.StandardOutput.Peek() -ge 0) {
    $line = $process.StandardOutput.ReadLine()
    Write-Host $line
}

Write-Host "`nProxy stderr:" -ForegroundColor Red
while ($process.StandardError.Peek() -ge 0) {
    $line = $process.StandardError.ReadLine()
    Write-Host $line
}

# Cleanup
$process.Kill()
Write-Host "`nTest complete." -ForegroundColor Cyan
