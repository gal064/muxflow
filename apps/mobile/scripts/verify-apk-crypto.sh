#!/usr/bin/env bash
# Check the compiled APK for the class names that Bouncy Castle loads at runtime.
set -euo pipefail

dexdump=${1:?usage: verify-apk-crypto.sh DEXDUMP APK}
apk=${2:?usage: verify-apk-crypto.sh DEXDUMP APK}

defined_classes=$("$dexdump" "$apk" | awk -F "'" '/^ *Class descriptor *:/ {print $2}')
required_classes=(
  'Lorg/bouncycastle/jce/provider/BouncyCastleProvider;'
  'Lorg/bouncycastle/jcajce/provider/keystore/BC$Mappings;'
  'Lorg/bouncycastle/jcajce/provider/keystore/bc/BcKeyStoreSpi$Std;'
  'Lorg/bouncycastle/jcajce/provider/asymmetric/EdEC$Mappings;'
  'Lorg/bouncycastle/jcajce/provider/asymmetric/edec/KeyFactorySpi$Ed25519;'
  'Lorg/bouncycastle/jcajce/provider/asymmetric/edec/SignatureSpi$Ed25519;'
  'Lorg/bouncycastle/jcajce/provider/asymmetric/edec/KeyAgreementSpi$X25519;'
)

for class in "${required_classes[@]}"; do
  if ! grep -Fxq -- "$class" <<< "$defined_classes"; then
    printf 'Release APK is missing crypto class %s\n' "$class" >&2
    exit 1
  fi
done
echo 'Release APK contains the required HTTPS and SSH crypto classes.'
