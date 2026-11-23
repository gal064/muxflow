# Bouncy Castle loads its JCA mappings and implementations by class name.
# SSH uses those algorithms, and Android TLS needs the replacement provider's BKS keystore.
-keep class org.bouncycastle.jcajce.provider.** { *; }
-keep class org.bouncycastle.jce.provider.** { *; }
