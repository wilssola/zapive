# Generates the self-signed Authenticode certificate the release workflow
# signs with. Run once, locally:
#   pwsh packaging/windows/make-self-signed-cert.ps1
# Then add the GitHub secrets: WINDOWS_PFX_BASE64 (printed below) and
# WINDOWS_PFX_PASSWORD (the password you typed). Keep the .pfx safe and
# never commit it.
$ErrorActionPreference = 'Stop'

$password = Read-Host -AsSecureString 'PFX password'
$cert = New-SelfSignedCertificate -Type CodeSigningCert -Subject 'CN=Zapive' `
    -KeyAlgorithm RSA -KeyLength 4096 -HashAlgorithm SHA256 `
    -NotAfter (Get-Date).AddYears(10) -CertStoreLocation Cert:\CurrentUser\My

$pfx = Join-Path $PWD 'zapive-selfsigned.pfx'
Export-PfxCertificate -Cert $cert -FilePath $pfx -Password $password | Out-Null
# The exportable copy is all we need; drop it from the user store.
Remove-Item "Cert:\CurrentUser\My\$($cert.Thumbprint)"

Write-Host "PFX written to $pfx"
Write-Host 'WINDOWS_PFX_BASE64:'
[Convert]::ToBase64String([IO.File]::ReadAllBytes($pfx))
