import CryptoKit
import Foundation
import Security

enum SshKeyStore {
  private static var query: [String: Any] {
    [kSecClass as String: kSecClassGenericPassword,
     kSecAttrService as String: "dev.muxflow.mobile.ssh",
     kSecAttrAccount as String: "ed25519",
     kSecAttrSynchronizable as String: false]
  }

  static func load() throws -> Curve25519.Signing.PrivateKey? {
    var request = query
    request[kSecReturnData as String] = true
    request[kSecMatchLimit as String] = kSecMatchLimitOne
    var item: CFTypeRef?
    let status = SecItemCopyMatching(request as CFDictionary, &item)
    if status == errSecItemNotFound { return nil }
    guard status == errSecSuccess, let data = item as? Data else {
      throw NSError(domain: "MuxflowKeychain", code: Int(status))
    }
    return try Curve25519.Signing.PrivateKey(rawRepresentation: data)
  }

  static func generate() throws -> String {
    let key = Curve25519.Signing.PrivateKey()
    let attributes: [String: Any] = [
      kSecValueData as String: key.rawRepresentation,
      kSecAttrAccessible as String: kSecAttrAccessibleWhenUnlockedThisDeviceOnly,
    ]
    let status = SecItemUpdate(query as CFDictionary, attributes as CFDictionary)
    if status == errSecItemNotFound {
      var item = query
      attributes.forEach { item[$0.key] = $0.value }
      let created = SecItemAdd(item as CFDictionary, nil)
      guard created == errSecSuccess else { throw NSError(domain: "MuxflowKeychain", code: Int(created)) }
    } else if status != errSecSuccess {
      throw NSError(domain: "MuxflowKeychain", code: Int(status))
    }
    return publicLine(key)
  }

  static func delete() throws {
    let status = SecItemDelete(query as CFDictionary)
    guard status == errSecSuccess || status == errSecItemNotFound else {
      throw NSError(domain: "MuxflowKeychain", code: Int(status))
    }
  }

  static func publicLine(_ key: Curve25519.Signing.PrivateKey) -> String {
    var blob = Data()
    for field in [Data("ssh-ed25519".utf8), key.publicKey.rawRepresentation] {
      var length = UInt32(field.count).bigEndian
      withUnsafeBytes(of: &length) { blob.append(contentsOf: $0) }
      blob.append(field)
    }
    return "ssh-ed25519 \(blob.base64EncodedString()) muxflow-mobile"
  }

  // RFC 8410 PKCS#8 wrapping. It stays native and is accepted by libssh2/OpenSSL.
  static func pem(_ key: Curve25519.Signing.PrivateKey) -> String {
    var der = Data([0x30, 0x2e, 0x02, 0x01, 0x00, 0x30, 0x05, 0x06,
                    0x03, 0x2b, 0x65, 0x70, 0x04, 0x22, 0x04, 0x20])
    der.append(key.rawRepresentation)
    return "-----BEGIN PRIVATE KEY-----\n\(der.base64EncodedString())\n-----END PRIVATE KEY-----\n"
  }
}
