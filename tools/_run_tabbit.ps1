param(
  [Parameter(Mandatory = $true)][string]$JsPath,
  [string]$TaskName = "Tabbit JS task",
  [string]$ReqId = "req1",
  [string]$Tab = "",
  [string]$TimeoutMs = ""
)
$cli = Join-Path $env:LOCALAPPDATA "Tabbit\LocalAgent\bin\tabbit-cli.exe"
$temp = [IO.Path]::GetTempFileName()
$js = [IO.File]::ReadAllText($JsPath, [Text.UTF8Encoding]::new($false))
[IO.File]::WriteAllText($temp, $js, [Text.UTF8Encoding]::new($false))
$extra = ""
if ($Tab -ne "") { $extra += " --tab $Tab" }
if ($TimeoutMs -ne "") { $extra += " --timeout-ms $TimeoutMs" }
cmd.exe /d /c "`"$cli`" nodejs --task `"$TaskName`"$extra --request-id $ReqId < `"$temp`""
Remove-Item $temp -ErrorAction SilentlyContinue
