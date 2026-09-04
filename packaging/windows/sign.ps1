# Authenticode-signs one file with the certificate in WINDOWS_PFX_BASE64.
# Self-signed: the signature embeds fine, but verification reports an
# untrusted root, so success is judged by the embedded signer, not Status.
param([Parameter(Mandatory)][string]$Path)
$ErrorActionPreference = 'Stop'

$tmp = if ($env:RUNNER_TEMP) { $env:RUNNER_TEMP } else { [IO.Path]::GetTempPath() }
$pfx = Join-Path $tmp 'zapive-signing.pfx'
[IO.File]::WriteAllBytes($pfx, [Convert]::FromBase64String($env:WINDOWS_PFX_BASE64))
try {
    $password = ConvertTo-SecureString $env:WINDOWS_PFX_PASSWORD -AsPlainText -Force
    $cert = Get-PfxCertificate -FilePath $pfx -Password $password
    $result = Set-AuthenticodeSignature -FilePath $Path -Certificate $cert `
        -HashAlgorithm SHA256 -TimestampServer 'http://timestamp.digicert.com'
    if (-not $result.SignerCertificate) {
        throw "signing $Path failed: $($result.StatusMessage)"
    }
    Write-Host "signed $Path"
} finally {
    Remove-Item $pfx -Force -ErrorAction SilentlyContinue
}
