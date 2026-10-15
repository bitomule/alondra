@testable import AlondraTransport
import CryptoKit
import Foundation
import Testing

struct IrohIdentityTests {
    /// alondra_e2e's `iroh_hello_fixed_vector`: the same bytes, and the daemon's own signature over them.
    @Test func helloBytesMatchTheDaemonAndItsSignatureVerifies() throws {
        let bytes = try IrohHello.bytes(macID: "mac-1", deviceID: "phone-1",
            deviceEndpoint: Data(repeating: 5, count: 32), macEndpoint: Data(repeating: 6, count: 32))
        #expect(Base64URL.encode(Data(SHA256.hash(data: bytes))) == "al6j5gmdVhTnWzDB4NVfDbpi1I5A6L2W_fgFBAE8HxY")
        let key = try P256.Signing.PrivateKey(rawRepresentation: Data(repeating: 1, count: 32)).publicKey
        let signature = try P256.Signing.ECDSASignature(rawRepresentation: Base64URL.decode(
            "HJ7leQPGMUQ8QITi_fWBSPfFikEOmuP_AwKnid9yc-Ey0chPvqBHOA9Lwz0-kEwRyHCW8LYnzK9PmQ_lo5ttLQ"))
        #expect(key.isValidSignature(signature, for: bytes))
    }

    @Test func theHelloSignsBothEndpointsWithThePairingKey() throws {
        let signing = P256.Signing.PrivateKey()
        let message = try IrohHello.message(macID: "mac-1", deviceID: "phone-1",
            deviceEndpoint: Data(repeating: 5, count: 32), macEndpoint: Data(repeating: 6, count: 32), signing: signing)
        let hello = try JSONDecoder().decode([String: String].self, from: message)
        #expect(Set(hello.keys) == ["device_id", "signature"])
        let signature = try P256.Signing.ECDSASignature(rawRepresentation: Base64URL.decode(try #require(hello["signature"])))
        let signed = try IrohHello.bytes(macID: "mac-1", deviceID: "phone-1",
            deviceEndpoint: Data(repeating: 5, count: 32), macEndpoint: Data(repeating: 6, count: 32))
        let swapped = try IrohHello.bytes(macID: "mac-1", deviceID: "phone-1",
            deviceEndpoint: Data(repeating: 6, count: 32), macEndpoint: Data(repeating: 5, count: 32))
        #expect(signing.publicKey.isValidSignature(signature, for: signed))
        #expect(!signing.publicKey.isValidSignature(signature, for: swapped))
    }

    /// The daemon's endpoint ID for the Ed25519 seed `[7; 32]` (iroh_tests.rs).
    @Test func theEndpointIDIsTheEd25519PublicKeyIrohDerives() {
        #expect(IrohIdentity(seed: Data(repeating: 7, count: 32)).endpointIDHex
            == "ea4a6c63e29c520abef5507b132ec5f9954776aebebe7b92421eea691446d22c")
    }

    @Test func eachPairingHasItsOwnStableEndpoint() throws {
        let signing = try P256.Signing.PrivateKey(rawRepresentation: Data(repeating: 9, count: 32))
        let first = IrohIdentity(pairingSigning: signing, macID: "mac-1")
        #expect(first == IrohIdentity(pairingSigning: signing, macID: "mac-1"))
        #expect(first != IrohIdentity(pairingSigning: signing, macID: "mac-2"))
        #expect(first != IrohIdentity(pairingSigning: P256.Signing.PrivateKey(), macID: "mac-1"))
    }

    /// What `PUT /v1/iroh-node` checks: the endpoint's signature over its domain and the device ID.
    @Test func theRelayBindingProvesTheEndpointSpeaksForThisDevice() throws {
        let identity = IrohIdentity(seed: Data(repeating: 7, count: 32))
        let binding = try JSONDecoder().decode([String: String].self, from: identity.nodeBinding(deviceID: "phone-1"))
        #expect(Set(binding.keys) == ["iroh_node_id", "proof"])
        #expect(binding["iroh_node_id"] == identity.endpointIDHex)
        let key = try Curve25519.Signing.PublicKey(rawRepresentation: identity.endpointID)
        let proof = try Base64URL.decode(try #require(binding["proof"]))
        #expect(key.isValidSignature(proof, for: Data("alondra-iroh-node-v1\0phone-1".utf8)))
        #expect(!key.isValidSignature(proof, for: Data("alondra-iroh-node-v1\0phone-2".utf8)))
    }

    @Test func theBootstrapNamesTheIrohRelayOnlyOverHTTPS() throws {
        func relay(_ json: String) throws -> URL? {
            try JSONDecoder().decode(RelayBootstrap.self, from: Data(json.utf8)).irohRelay
        }
        #expect(try relay(#"{"protocol":1,"epoch":"e","iroh_relay":"https://iroh.example.com"}"#)
            == URL(string: "https://iroh.example.com"))
        #expect(try relay(#"{"protocol":1,"epoch":"e"}"#) == nil)
        #expect(try relay(#"{"protocol":1,"epoch":"e","iroh_relay":"http://iroh.example.com"}"#) == nil)
        #expect(try relay(#"{"protocol":1,"epoch":"e","iroh_relay":7}"#) == nil)
    }

    @Test func aMacAddressNeedsAValidEndpointAndSomewhereToReachIt() {
        let id = String(repeating: "ab", count: 32)
        #expect(IrohMacAddress(endpointID: id, relay: URL(string: "https://iroh.example.com")) != nil)
        #expect(IrohMacAddress(endpointID: id, relay: nil, directAddresses: ["127.0.0.1:4"]) != nil)
        #expect(IrohMacAddress(endpointID: id, relay: nil) == nil)
        #expect(IrohMacAddress(endpointID: id.uppercased(), relay: URL(string: "https://iroh.example.com")) == nil)
    }
}
