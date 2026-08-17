# Creates a synthetic 100k-entry directory for `neutron --bench`.
#
# Real directories that large are rare, and the ones that exist (WinSxS) have
# atypical name distributions. This gives a stable, reproducible baseline:
# mixed extensions and numeric runs so natural-order sorting has real work.
#
#     powershell -ExecutionPolicy Bypass -File scripts/make-bench-dir.ps1
#
# Takes a few minutes. Delete the directory when finished — it is 100k files.

$dir = 'C:\Users\Razeen\.neutron-bench'
if (Test-Path $dir) { Remove-Item $dir -Recurse -Force }
New-Item -ItemType Directory -Path $dir | Out-Null
# Mixed names so natural sort has real work: numeric runs, varied extensions,
# and some directories interleaved.
$exts = @('txt','rs','png','log','json','dll')
for ($i = 1; $i -le 100000; $i++) {
  $e = $exts[$i % 6]
  [System.IO.File]::Create("$dir\item$i.$e").Close()
}
for ($i = 1; $i -le 200; $i++) { New-Item -ItemType Directory -Path "$dir\folder$i" | Out-Null }
Write-Output ("created: " + (Get-ChildItem $dir | Measure-Object).Count)
