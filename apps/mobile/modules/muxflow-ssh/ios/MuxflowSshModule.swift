import ExpoModulesCore
import Foundation

public final class MuxflowSshModule: Module {
  private let registry = DispatchQueue(label: "dev.muxflow.ssh.registry")
  private var transports: [Target: MFSSHTransport] = [:]
  private var channels: [String: MFSSHTransport] = [:]

  private struct Target: Hashable {
    let host: String
    let port: Int
    let user: String
  }

  public func definition() -> ModuleDefinition {
    Name("MuxflowSsh")
    Events("onSshEvent")

    AsyncFunction("generateKeyPair") { (promise: Promise) in
      self.registry.async {
        do { promise.resolve(["publicKeyOpenSsh": try SshKeyStore.generate()]) }
        catch { promise.reject("E_KEYCHAIN", "Could not store the SSH key") }
      }
    }
    AsyncFunction("getPublicKey") { (promise: Promise) in
      self.registry.async {
        do { promise.resolve(try SshKeyStore.load().map(SshKeyStore.publicLine)) }
        catch { promise.reject("E_KEYCHAIN", "Could not read the SSH key") }
      }
    }
    AsyncFunction("deleteKeyPair") { (promise: Promise) in
      self.registry.async {
        do { try SshKeyStore.delete(); promise.resolve() }
        catch { promise.reject("E_KEYCHAIN", "Could not delete the SSH key") }
      }
    }
    AsyncFunction("connect") { (identifier: String, raw: [String: Any], command: String, fingerprint: String?, promise: Promise) in
      self.registry.async {
        guard self.channels[identifier] == nil,
              let host = raw["host"] as? String, !host.isEmpty,
              // Expo hydrates numbers in [String: Any] as Double.
              let rawPort = raw["port"] as? Double,
              let port = Int(exactly: rawPort), (1...65535).contains(port),
              let user = raw["user"] as? String, !user.isEmpty else {
          promise.reject("E_TARGET", "Invalid SSH target or duplicate channel")
          return
        }
        let target = Target(host: host.lowercased(), port: port, user: user)
        do {
          var transport = self.transports[target]
          if transport?.acceptingChannels != true {
            let pem = try SshKeyStore.load().map(SshKeyStore.pem)
            transport = MFSSHTransport(host: host, port: port, user: user, privatePem: pem) { [weak self] event in
              self?.receive(event)
            }
            self.transports[target] = transport
          }
          guard let transport else {
            promise.reject("E_SOCKET", "Could not create SSH transport")
            return
          }
          self.channels[identifier] = transport
          transport.open(identifier, command: command, fingerprint: fingerprint)
          promise.resolve()
        } catch {
          promise.reject("E_KEYCHAIN", "Could not read the SSH key")
        }
      }
    }
    AsyncFunction("trustHostKey") { (identifier: String, fingerprint: String, promise: Promise) in
      self.registry.async {
        self.channels[identifier]?.trust(identifier, fingerprint: fingerprint)
        promise.resolve()
      }
    }
    AsyncFunction("write") { (identifier: String, base64: String, promise: Promise) in
      self.registry.async {
        guard let data = Data(base64Encoded: base64), let transport = self.channels[identifier] else {
          promise.reject("E_WRITE", "Invalid payload or closed SSH channel")
          return
        }
        transport.write(identifier, data: data) { error in
          if let error { promise.reject("E_WRITE", error) }
          else { promise.resolve() }
        }
      }
    }
    AsyncFunction("close") { (identifier: String, promise: Promise) in
      self.registry.async {
        self.channels[identifier]?.close(identifier)
        promise.resolve()
      }
    }
    OnDestroy {
      self.registry.async {
        for (identifier, transport) in self.channels { transport.close(identifier) }
      }
    }
  }

  private func receive(_ event: [String: Any]) {
    registry.async {
      if event["type"] as? String == "closed", let identifier = event["connectionId"] as? String {
        self.channels.removeValue(forKey: identifier)
        self.transports = self.transports.filter { $0.value.acceptingChannels }
      }
      self.sendEvent("onSshEvent", event)
    }
  }
}
