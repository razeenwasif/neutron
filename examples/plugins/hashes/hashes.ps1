# Reads one request from standard input and writes newline-delimited responses.
#
# The whole protocol, in a page: read a line, do the work, print what you want
# the file manager to do.

$ErrorActionPreference = 'Stop'

function Respond($object) {
    # -Compress keeps each response on one line, which the protocol requires.
    Write-Output ($object | ConvertTo-Json -Compress)
}

try {
    $request = [Console]::In.ReadLine() | ConvertFrom-Json

    $lines = @()
    foreach ($path in $request.selection) {
        # Folders are skipped rather than refused: selecting a folder along
        # with some files is normal, and failing the whole command over it
        # would be unhelpful.
        if (Test-Path -LiteralPath $path -PathType Leaf) {
            $hash = (Get-FileHash -LiteralPath $path -Algorithm SHA256).Hash.ToLower()
            $lines += "$hash  $(Split-Path -Leaf $path)"
        }
    }

    if ($lines.Count -eq 0) {
        Respond @{ type = 'error'; message = 'nothing in the selection is a file' }
        exit 1
    }

    # WriteAllLines rather than Set-Content -Encoding utf8: Windows PowerShell's
    # "utf8" writes a byte-order mark, and a BOM at the start of the first line
    # makes the file unreadable to `sha256sum -c`.
    $output = Join-Path $request.folder 'SHA256SUMS.txt'
    [System.IO.File]::WriteAllLines($output, $lines, (New-Object System.Text.UTF8Encoding $false))

    $count = $lines.Count
    $noun = if ($count -eq 1) { 'file' } else { 'files' }
    Respond @{ type = 'status'; text = "$count $noun hashed into SHA256SUMS.txt" }
    # The folder gained a file. Neutron is watching it and would notice anyway,
    # but a plugin should say so rather than rely on that.
    Respond @{ type = 'refresh' }
}
catch {
    # The message reaches the status bar as the plugin's own words, so it is
    # worth making it a sentence.
    Respond @{ type = 'error'; message = $_.Exception.Message }
    exit 1
}
