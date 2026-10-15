import CryptoKit
import Foundation
func hex(_ value: String) -> Data {
  Data(stride(from: 0, to: value.count, by: 2).map {
    UInt8(value.dropFirst($0).prefix(2), radix: 16)!
  })
}
func b64(_ value: String) -> Data {
  var v = value.replacingOccurrences(of: "-", with: "+").replacingOccurrences(of: "_", with: "/")
  while v.count % 4 != 0 { v += "=" }
  return Data(base64Encoded: v)!
}
@main struct Check {
  static func main() throws {
    let raw = try Data(contentsOf: URL(fileURLWithPath: CommandLine.arguments[1]))
    let fixture = try JSONSerialization.jsonObject(with: raw) as! [String: Any]
    let envelope = fixture["envelope"] as! [String: Any]
    let key = try P256.KeyAgreement.PrivateKey(rawRepresentation: hex(fixture["recipient_private_key_hex"] as! String))
    let info = Data("buzz/nsec-backup/v1".utf8)
    let aad = hex(fixture["aad_hex"] as! String)
    let secret = hex(fixture["nostr_secret_key_hex"] as! String)
    var recipient = try HPKE.Recipient(privateKey: key, ciphersuite: .P256_SHA256_AES_GCM_256, info: info, encapsulatedKey: b64(envelope["enc"] as! String))
    let fixturePlaintext = try recipient.open(b64(envelope["ciphertext"] as! String), authenticating: aad)
    precondition(fixturePlaintext == secret)
    let sealed = try HostedHpkeBackup.seal(recipient: key.publicKey.x963Representation, plaintext: secret, aad: aad)
    var roundtrip = try HPKE.Recipient(privateKey: key, ciphersuite: .P256_SHA256_AES_GCM_256, info: info, encapsulatedKey: sealed.enc)
    let roundtripPlaintext = try roundtrip.open(sealed.ciphertext, authenticating: aad)
    precondition(roundtripPlaintext == secret)
    var tampered = try HPKE.Recipient(privateKey: key, ciphersuite: .P256_SHA256_AES_GCM_256, info: info, encapsulatedKey: sealed.enc)
    do { _ = try tampered.open(sealed.ciphertext, authenticating: Data("wrong".utf8)); fatalError("accepted incorrect AAD") } catch {}
    print("Fixture decrypt, seal/open, and AAD rejection passed")
  }
}
