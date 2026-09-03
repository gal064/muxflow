package dev.muxflow.ssh

import android.content.Context
import android.content.SharedPreferences
import android.util.Base64
import androidx.security.crypto.EncryptedSharedPreferences
import androidx.security.crypto.MasterKey
import java.io.ByteArrayOutputStream
import java.security.KeyPair
import java.security.MessageDigest
import java.security.PublicKey
import java.security.SecureRandom
import java.security.Security
import net.schmizz.sshj.common.Buffer
import net.schmizz.sshj.common.Ed25519KeyFactory
import net.schmizz.sshj.common.SecurityUtils
import org.bouncycastle.crypto.params.Ed25519PrivateKeyParameters
import org.bouncycastle.jce.provider.BouncyCastleProvider

/**
 * Android ships a cut-down provider under the name "BC" that has neither ed25519 key factories nor
 * the algorithms sshj probes for, and sshj will not register a provider whose name is already
 * taken. Replacing it once, before any crypto happens, is what makes ed25519 work below API 33.
 */
internal object SshSecurity {
  @Volatile private var registered = false

  @Synchronized
  fun ensureBouncyCastle() {
    if (registered) {
      return
    }
    if (Security.getProvider(BouncyCastleProvider.PROVIDER_NAME) !is BouncyCastleProvider) {
      Security.removeProvider(BouncyCastleProvider.PROVIDER_NAME)
      // Appended rather than inserted first, so platform TLS keeps using the platform providers.
      Security.addProvider(BouncyCastleProvider())
    }
    // Pin sshj to it explicitly; this also short-circuits sshj's own registration attempt.
    SecurityUtils.setSecurityProvider(BouncyCastleProvider.PROVIDER_NAME)
    registered = true
  }
}

/**
 * The phone's own SSH identity: one ed25519 key pair, generated on device and kept in
 * [EncryptedSharedPreferences] (AES-256, Keystore-backed master key). The private key is never
 * returned to JavaScript and never logged (design doc §14).
 *
 * On disk the private key is an unencrypted OpenSSH private key ("openssh-key-v1"), the same
 * container `ssh-keygen -t ed25519` writes. [encodeOpenSshPrivateKey] and [decodeOpenSshPrivateKey]
 * are inverses of each other and [generate] round-trips what it just wrote before storing it, so a
 * container this class cannot read again never reaches storage.
 */
internal object SshKeyStore {
  private const val PREFS_FILE = "muxflow.ssh"

  /** Preference key holding the OpenSSH private key PEM (design doc §6.2). */
  private const val PRIVATE_KEY_PREF = "muxflow.ssh.private"

  const val KEY_COMMENT = "muxflow-mobile"
  private const val KEY_TYPE = "ssh-ed25519"
  private const val SEED_LENGTH = 32
  private const val PUBLIC_KEY_LENGTH = 32

  private val AUTH_MAGIC = "openssh-key-v1".toByteArray(Charsets.US_ASCII) + 0

  private const val PEM_HEADER = "-----BEGIN OPENSSH PRIVATE KEY-----"
  private const val PEM_FOOTER = "-----END OPENSSH PRIVATE KEY-----"
  private const val PEM_LINE_LENGTH = 70

  @Volatile private var cachedPrefs: SharedPreferences? = null

  @Synchronized
  private fun prefs(context: Context): SharedPreferences {
    cachedPrefs?.let { return it }
    val appContext = context.applicationContext
    val masterKey =
      MasterKey.Builder(appContext).setKeyScheme(MasterKey.KeyScheme.AES256_GCM).build()
    val created =
      EncryptedSharedPreferences.create(
        appContext,
        PREFS_FILE,
        masterKey,
        EncryptedSharedPreferences.PrefKeyEncryptionScheme.AES256_SIV,
        EncryptedSharedPreferences.PrefValueEncryptionScheme.AES256_GCM,
      )
    cachedPrefs = created
    return created
  }

  /**
   * Generates a fresh ed25519 key pair, replacing any existing one, and returns the OpenSSH public
   * key line ("ssh-ed25519 AAAA… muxflow-mobile").
   */
  fun generate(context: Context): String {
    SshSecurity.ensureBouncyCastle()
    val seed = ByteArray(SEED_LENGTH)
    SecureRandom().nextBytes(seed)
    val publicKeyBinary = Ed25519PrivateKeyParameters(seed, 0).generatePublicKey().encoded
    val pem = encodeOpenSshPrivateKey(seed, publicKeyBinary)

    // Refuse to store a container we cannot read back: a broken write would otherwise only show up
    // later as an unexplained authentication failure.
    val reread = decodeOpenSshPrivateKey(pem)
    require(reread.seed.contentEquals(seed) && reread.publicKeyBinary.contentEquals(publicKeyBinary)) {
      "generated private key did not survive a round trip"
    }

    if (!prefs(context).edit().putString(PRIVATE_KEY_PREF, pem).commit()) {
      throw SshKeyStoreException("Could not save the SSH key on this device.")
    }
    return openSshPublicKeyLine(publicKeyBinary)
  }

  /** The stored public key as an OpenSSH line, or null when this device has no key yet. */
  fun publicKeyOpenSsh(context: Context): String? {
    val pem = prefs(context).getString(PRIVATE_KEY_PREF, null) ?: return null
    return openSshPublicKeyLine(decodeOpenSshPrivateKey(pem).publicKeyBinary)
  }

  fun delete(context: Context) {
    if (!prefs(context).edit().remove(PRIVATE_KEY_PREF).commit()) {
      throw SshKeyStoreException("Could not remove the SSH key from this device.")
    }
  }

  /**
   * The stored key pair as JCA keys, or null when this phone has no key. A missing key is not an
   * error: a host reached over Tailscale SSH authenticates the phone by its tailnet identity and
   * accepts the `none` method, so the transport can log in without one.
   */
  fun keyPairOrNull(context: Context): KeyPair? {
    SshSecurity.ensureBouncyCastle()
    val pem = prefs(context).getString(PRIVATE_KEY_PREF, null) ?: return null
    val decoded = decodeOpenSshPrivateKey(pem)
    return KeyPair(
      Ed25519KeyFactory.getPublicKey(decoded.publicKeyBinary),
      Ed25519KeyFactory.getPrivateKey(decoded.seed),
    )
  }

  fun openSshPublicKeyLine(publicKeyBinary: ByteArray): String {
    val blob = PlainSshBlob { putSshString(KEY_TYPE.toByteArray(Charsets.UTF_8)); putSshString(publicKeyBinary) }
    return "$KEY_TYPE ${Base64.encodeToString(blob, Base64.NO_WRAP)} $KEY_COMMENT"
  }

  /**
   * The OpenSSH SHA-256 fingerprint of a public key: "SHA256:" plus the unpadded base64 of the
   * SHA-256 digest of the key's SSH wire encoding, exactly what `ssh-keygen -lf` prints.
   */
  fun sha256Fingerprint(key: PublicKey): String {
    SshSecurity.ensureBouncyCastle()
    val blob = Buffer.PlainBuffer().putPublicKey(key).compactData
    val digest = MessageDigest.getInstance("SHA-256").digest(blob)
    return "SHA256:" + Base64.encodeToString(digest, Base64.NO_PADDING or Base64.NO_WRAP)
  }

  internal class DecodedKey(val seed: ByteArray, val publicKeyBinary: ByteArray)

  // ---------------------------------------------------------------------------------------------
  // openssh-key-v1 container, cipher "none". Layout per PROTOCOL.key in the OpenSSH sources.
  // ---------------------------------------------------------------------------------------------

  internal fun encodeOpenSshPrivateKey(seed: ByteArray, publicKeyBinary: ByteArray): String {
    val publicBlob = PlainSshBlob {
      putSshString(KEY_TYPE.toByteArray(Charsets.UTF_8))
      putSshString(publicKeyBinary)
    }
    // Two identical check integers let a decrypting reader detect a wrong passphrase; with cipher
    // "none" they are only a consistency marker, but the format still requires them.
    val checkInt = SecureRandom().nextInt()
    val privateSection = ByteArrayOutputStream()
    privateSection.putUint32(checkInt)
    privateSection.putUint32(checkInt)
    privateSection.putSshString(KEY_TYPE.toByteArray(Charsets.UTF_8))
    privateSection.putSshString(publicKeyBinary)
    privateSection.putSshString(seed + publicKeyBinary)
    privateSection.putSshString(KEY_COMMENT.toByteArray(Charsets.UTF_8))
    // Pad to the cipher block size (8 for "none") with 1, 2, 3, …
    var pad = 1
    while (privateSection.size() % 8 != 0) {
      privateSection.write(pad)
      pad += 1
    }

    val container = ByteArrayOutputStream()
    container.write(AUTH_MAGIC)
    container.putSshString("none".toByteArray(Charsets.US_ASCII)) // ciphername
    container.putSshString("none".toByteArray(Charsets.US_ASCII)) // kdfname
    container.putSshString(ByteArray(0)) // kdfoptions
    container.putUint32(1) // number of keys
    container.putSshString(publicBlob)
    container.putSshString(privateSection.toByteArray())

    val body = Base64.encodeToString(container.toByteArray(), Base64.NO_WRAP)
    return buildString {
      append(PEM_HEADER).append('\n')
      var index = 0
      while (index < body.length) {
        val end = minOf(index + PEM_LINE_LENGTH, body.length)
        append(body, index, end).append('\n')
        index = end
      }
      append(PEM_FOOTER).append('\n')
    }
  }

  internal fun decodeOpenSshPrivateKey(pem: String): DecodedKey {
    val body =
      pem
        .lineSequence()
        .map { it.trim() }
        .filter { it.isNotEmpty() && it != PEM_HEADER && it != PEM_FOOTER }
        .joinToString("")
    val container =
      try {
        Base64.decode(body, Base64.DEFAULT)
      } catch (e: IllegalArgumentException) {
        throw SshKeyStoreException("The stored SSH key is not readable.", e)
      }
    val reader = SshBlobReader(container)
    if (!reader.readRaw(AUTH_MAGIC.size).contentEquals(AUTH_MAGIC)) {
      throw SshKeyStoreException("The stored SSH key is not an OpenSSH private key.")
    }
    val cipherName = reader.readSshString().toString(Charsets.US_ASCII)
    val kdfName = reader.readSshString().toString(Charsets.US_ASCII)
    reader.readSshString() // kdfoptions
    if (cipherName != "none" || kdfName != "none") {
      throw SshKeyStoreException("The stored SSH key is encrypted, which this app never writes.")
    }
    if (reader.readUint32() != 1) {
      throw SshKeyStoreException("The stored SSH key holds more than one key.")
    }
    reader.readSshString() // public key blob; the authoritative copy is inside the private section
    val privateSection = SshBlobReader(reader.readSshString())
    if (privateSection.readUint32() != privateSection.readUint32()) {
      throw SshKeyStoreException("The stored SSH key is corrupt.")
    }
    val keyType = privateSection.readSshString().toString(Charsets.US_ASCII)
    if (keyType != KEY_TYPE) {
      throw SshKeyStoreException("The stored SSH key is a $keyType key, not $KEY_TYPE.")
    }
    val publicKeyBinary = privateSection.readSshString()
    val privateKeyBinary = privateSection.readSshString()
    if (
      publicKeyBinary.size != PUBLIC_KEY_LENGTH ||
        privateKeyBinary.size != SEED_LENGTH + PUBLIC_KEY_LENGTH
    ) {
      throw SshKeyStoreException("The stored SSH key has the wrong length.")
    }
    return DecodedKey(privateKeyBinary.copyOfRange(0, SEED_LENGTH), publicKeyBinary)
  }

  private fun ByteArrayOutputStream.putUint32(value: Int) {
    write((value ushr 24) and 0xff)
    write((value ushr 16) and 0xff)
    write((value ushr 8) and 0xff)
    write(value and 0xff)
  }

  private fun ByteArrayOutputStream.putSshString(value: ByteArray) {
    putUint32(value.size)
    write(value, 0, value.size)
  }

  private inline fun PlainSshBlob(build: ByteArrayOutputStream.() -> Unit): ByteArray =
    ByteArrayOutputStream().apply(build).toByteArray()

  private class SshBlobReader(private val bytes: ByteArray) {
    private var offset = 0

    fun readRaw(length: Int): ByteArray {
      if (length < 0 || offset + length > bytes.size) {
        throw SshKeyStoreException("The stored SSH key is truncated.")
      }
      val slice = bytes.copyOfRange(offset, offset + length)
      offset += length
      return slice
    }

    fun readUint32(): Int {
      val raw = readRaw(4)
      return ((raw[0].toInt() and 0xff) shl 24) or
        ((raw[1].toInt() and 0xff) shl 16) or
        ((raw[2].toInt() and 0xff) shl 8) or
        (raw[3].toInt() and 0xff)
    }

    fun readSshString(): ByteArray = readRaw(readUint32())
  }
}

internal class SshKeyStoreException(message: String, cause: Throwable? = null) :
  RuntimeException(message, cause)
