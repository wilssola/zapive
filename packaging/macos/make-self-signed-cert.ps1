# Windows counterpart of make-self-signed-cert.sh: produces the same
# PKCS#12 the macOS job imports, without needing openssl. Run once:
#   pwsh packaging/macos/make-self-signed-cert.ps1
# Then add the GitHub secrets: MACOS_P12_BASE64 (printed below) and
# MACOS_P12_PASSWORD (the password you typed). Keep the .p12 safe and
# never commit it.
$ErrorActionPreference = 'Stop'

$password = Read-Host -AsSecureString 'P12 password'
# The subject CN is the identity codesign looks up in the workflow.
$cert = New-SelfSignedCertificate -Type CodeSigningCert -Subject 'CN=Zapive Self-Signed' `
    -KeyAlgorithm RSA -KeyLength 4096 -HashAlgorithm SHA256 `
    -NotAfter (Get-Date).AddYears(10) -CertStoreLocation Cert:\CurrentUser\My

$p12 = Join-Path $PWD 'zapive-selfsigned.p12'
# AES256: Security.framework reads it, and so does OpenSSL 3 without the
# legacy provider, unlike the TripleDES default.
Export-PfxCertificate -Cert $cert -FilePath $p12 -Password $password `
    -CryptoAlgorithmOption AES256_SHA256 | Out-Null
Remove-Item "Cert:\CurrentUser\My\$($cert.Thumbprint)"

Write-Host "P12 written to $p12"
Write-Host 'MACOS_P12_BASE64:'
[Convert]::ToBase64String([IO.File]::ReadAllBytes($p12))
