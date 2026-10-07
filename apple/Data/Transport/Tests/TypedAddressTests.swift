import AlondraTransport
import Foundation
import Testing

@Suite struct TypedAddressTests {
    @Test(arguments: [
        ("192.168.1.20", DirectEndpoint(host: "192.168.1.20", port: 47810, kind: .localNetwork)),
        (" 192.168.1.20:48211 ", DirectEndpoint(host: "192.168.1.20", port: 48211, kind: .localNetwork)),
        ("http://192.168.1.20/", DirectEndpoint(host: "192.168.1.20", port: 47810, kind: .localNetwork)),
        ("100.101.2.3", DirectEndpoint(host: "100.101.2.3", port: 47810, kind: .tailscale)),
        ("Studio.tail1.ts.net", DirectEndpoint(host: "Studio.tail1.ts.net", port: 47810, kind: .tailscale)),
        ("studio.local:47811", DirectEndpoint(host: "studio.local", port: 47811, kind: .localNetwork)),
        ("[fd7a:115c:a1e0::1]:47810", DirectEndpoint(host: "fd7a:115c:a1e0::1", port: 47810, kind: .localNetwork)),
    ])
    func aTypedAddressBecomesAnEndpoint(text: String, expected: DirectEndpoint) {
        #expect(DirectEndpoint(typed: text) == expected)
    }

    @Test(arguments: ["", "   ", "studio:abc", "studio:0", "studio:70000", "fe80::1%en0", "[fd7a::1]x", "two words"])
    func nonsenseIsRefused(text: String) {
        #expect(DirectEndpoint(typed: text) == nil)
    }
}
