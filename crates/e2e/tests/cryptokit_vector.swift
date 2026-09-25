import Foundation
import CryptoKit
func b64(_ input: String) -> Data {
    let base = input.replacingOccurrences(of: "-", with: "+").replacingOccurrences(of: "_", with: "/")
    return Data(base64Encoded: base + String(repeating: "=", count: (4 - base.count % 4) % 4))!
}
let envelopeJSON = #"{"v":1,"route":"mac-1","sender":"phone-1","recipient":"mac-1","request_id":"op-123","kind":"request","expires_at_ms":1800000000000,"ephemeral_key":"BFkat3HrvP1tnLkJTRBlKK3Rpp1EwsH2J_CJ7Fi5xhrfn05qvw0EXAxpOjxorXyXynK-ZN70om_s0mPdmKkngPA","nonce":"BAQEBAQEBAQEBAQE","ciphertext":"DZOlUahLUjAlbbrvDcKNoy6JiIX89Yt-e3zvpAOlHGXV04zSBt-P9bszLP6uIsCXziRbUgaxIZ55V70DU4jmd-zOju3sTR8QEaapNa0ZBaescA-k-Fc8XaQZtQ","signature":"G0xSJBObtOnOYf_59G2YvwbtRsIHZ8O8qbE2nXP7wRQbknF-95BMJW6bk7UXah7QXWNjc7Faaqt013anjExzoQ"}"#
let e = try JSONSerialization.jsonObject(with: Data(envelopeJSON.utf8)) as! [String: Any]
var header = Data("alondra-e2e-envelope-v1".utf8)
func frame(_ data: Data) { var n = UInt32(data.count).bigEndian; withUnsafeBytes(of: &n) { header.append(contentsOf: $0) }; header.append(data) }
frame(Data([1]))
for key in ["route", "sender", "recipient", "request_id", "kind"] { frame(Data((e[key] as! String).utf8)) }
var expiry = UInt64(e["expires_at_ms"] as! Int).bigEndian
withUnsafeBytes(of: &expiry) { frame(Data($0)) }
frame(b64(e["ephemeral_key"] as! String)); frame(b64(e["nonce"] as! String))
let recipient = try P256.KeyAgreement.PrivateKey(rawRepresentation: Data(repeating: 2, count: 32))
let ephemeral = try P256.KeyAgreement.PublicKey(x963Representation: b64(e["ephemeral_key"] as! String))
let shared = try recipient.sharedSecretFromKeyAgreement(with: ephemeral)
let salt = Data(SHA256.hash(data: header))
let key = shared.hkdfDerivedSymmetricKey(using: SHA256.self, salt: salt, sharedInfo: Data("alondra-e2e-key-v1".utf8), outputByteCount: 32)
let keyData = key.withUnsafeBytes { Data($0) }
let keyB64 = keyData.base64EncodedString().replacingOccurrences(of: "+", with: "-").replacingOccurrences(of: "/", with: "_").replacingOccurrences(of: "=", with: "")
assert(keyB64 == "2n5soF2KkcWhzbLFGuoKGWI8vSRrsr4zpsCf0_bkADQ")
let nonce = b64(e["nonce"] as! String)
let combined = nonce + b64(e["ciphertext"] as! String)
let box = try ChaChaPoly.SealedBox(combined: combined)
let plaintext = try ChaChaPoly.open(box, using: key, authenticating: header)
assert(String(data: plaintext, encoding: .utf8) == #"{"method":"POST","path":"/v1/questions/q1/answer","body":{"answer":"hola"}}"#)
let resealed = try ChaChaPoly.seal(plaintext, using: key, nonce: try ChaChaPoly.Nonce(data: nonce), authenticating: header)
assert(resealed.ciphertext + resealed.tag == b64(e["ciphertext"] as! String))
var signed = header
var cipherCount = UInt32(b64(e["ciphertext"] as! String).count).bigEndian
withUnsafeBytes(of: &cipherCount) { signed.append(contentsOf: $0) }
signed.append(b64(e["ciphertext"] as! String))
let pub = try P256.Signing.PublicKey(x963Representation: b64("BG_wO5SSQc4drdQ1GeaWDgqFtBppoFwygQOqK84VlMoWPE91OlW_AdxT9sCwx-7ni0DG_30lqW4igrmJzvccFEo"))
let sig = try P256.Signing.ECDSASignature(rawRepresentation: b64(e["signature"] as! String))
assert(pub.isValidSignature(sig, for: signed))
print("CryptoKit verified HKDF key, ChaChaPoly encryption/decryption, and P-256 signature")
