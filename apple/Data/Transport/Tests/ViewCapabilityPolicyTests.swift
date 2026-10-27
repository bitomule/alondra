@testable import AlondraTransport
import Testing

@Suite struct ViewCapabilityPolicyTests {
    @Test func negotiatedCapabilitiesPassBothDirectAndSealedViewPolicies() {
        let query = "protocol=1&client=ios&lang=en&capabilities=provider_accounts%2Cmodel_selection"
        #expect(EnvelopeRequestPolicy.validClientContractPath(method: "GET", path: "/views/accounts?" + query))
        #expect(EnvelopeRequestPolicy.validViewsPath(method: "GET", path: "/views/accounts?" + query, hasBody: false))
        #expect(EnvelopeRequestPolicy.viewsChangeWait("/changes?" + query + "&wait=50") == 50)
    }
    @Test(arguments: ["Provider", "provider_accounts,,model_selection", "a%2Fb", "a%0Ab", String(repeating: "a", count: 257)])
    func malformedCapabilitiesCannotReachDirectOrRelayPaths(_ capabilities: String) {
        let query = "protocol=1&client=ios&lang=en&capabilities=" + capabilities
        #expect(!EnvelopeRequestPolicy.validClientContractPath(method: "GET", path: "/views/accounts?" + query))
        #expect(EnvelopeRequestPolicy.viewsChangeWait("/changes?" + query + "&wait=50") == nil)
    }
}
