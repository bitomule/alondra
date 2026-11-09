@testable import AlondraTransport
import Foundation
import Testing

@Suite struct RegistrationFailureTests {
    @Test(arguments: [
        (IrohCarrierError.unavailable as any Error, RegistrationFailure.Reason.unavailable),
        (IrohCarrierError.noAnswer as any Error, .unavailable),
        (IrohCarrierError.refused("untrusted diagnostic") as any Error, .rejected),
        (IrohLinkError.notSent as any Error, .unavailable),
        (IrohLinkError.noAnswer as any Error, .unavailable),
        (DirectRouteError.unreachable as any Error, .unavailable),
        (DeadlineExceeded() as any Error, .unavailable),
        (DirectSessionError.deviceRevoked as any Error, .approvalRequired),
        (DirectRouteError.identityChanged as any Error, .identityChanged),
        (DirectRouteError.localNetworkDenied as any Error, .localNetworkDenied),
        (AccountDeviceError.removedFromWeb as any Error, .accountDeviceRemoved),
        (RelayWireError.invalidSignature as any Error, .incompatible),
    ])
    func typedTransportFailuresKeepTheirCauseAndStage(error: any Error, reason: RegistrationFailure.Reason) {
        for operation in [RegistrationFailure.Operation.mac, .activityKey] {
            #expect(RegistrationFailure.classify(error, operation: operation) == RegistrationFailure(operation: operation, reason: reason))
        }
    }

    @Test(arguments: [
        ("invalid_registration_body", 422, RegistrationFailure.Reason.invalidRequest(.invalidBody)),
        ("unknown_registration_field", 422, .invalidRequest(.unknownField)),
        ("invalid_push_binding", 422, .invalidRequest(.invalidBinding)),
        ("invalid_quiet_hours", 422, .invalidRequest(.invalidQuietHours)),
        ("invalid_preferences_revision", 422, .invalidRequest(.invalidRevision)),
        ("missing_quiet_hours", 422, .invalidRequest(.missingQuietHours)),
        ("invalid_activity_key", 422, .invalidRequest(.invalidActivityKey)),
        ("device_not_enrolled", 409, .notEnrolled),
        ("stale_push_preferences", 409, .stalePreferences(77)),
        ("unsupported_registration_route", 404, .incompatible),
        ("invalid_registration_method", 400, .invalidRequest(.invalidMethod)),
        ("device_approval_revoked", 403, .approvalRequired),
        ("operation_capacity", 429, .capacity),
        ("registration_expired", 410, .expired),
    ])
    func macContractRetainsReasonAndOperation(kind: String, status: Int, reason: RegistrationFailure.Reason) async throws {
        let body = try JSONSerialization.data(withJSONObject: ["kind": kind, "current_revision": 77, "error": "untrusted diagnostic"])
        let transport = RegistrationResponses(status: status, body: body)
        let api = AlondraAPIClient(serviceURL: URL(string: "https://example.test")!, deviceID: "client", operations: transport)
        for operation in [RegistrationFailure.Operation.mac, .activityKey] {
            do {
                if operation == .mac {
                    try await api.registerServicePush(token: "test-token", environment: "sandbox", binding: String(repeating: "A", count: 43), preferencesRevision: 1)
                } else { try await api.giveActivityKey("test-key") }
                Issue.record("Rejection accepted")
            } catch let failure as RegistrationFailure {
                #expect(failure == RegistrationFailure(operation: operation, reason: reason))
                #expect(!failure.message.contains("untrusted diagnostic"))
            }
        }
    }

    @Test(arguments: [
        (403, #"{"error":"device_revoked"}"#, RegistrationFailure.Reason.approvalRequired),
        (404, #"{"error":"old route"}"#, .incompatible),
        (429, #"{}"#, .capacity),
        (410, #"{"kind":"outcome_unknown","error":"outcome_unknown","reason":"expired"}"#, .expired),
        (410, #"{"kind":"outcome_unknown","reason":"future_reason"}"#, .rejected),
        (503, #"{}"#, .unavailable),
        (422, #"{"kind":"future_reason","error":"device_not_enrolled"}"#, .rejected),
        (409, #"{"kind":"stale_push_preferences","current_revision":-1}"#, .rejected),
        (409, #"{"kind":"stale_push_preferences","current_revision":9007199254740991}"#, .incompatible),
        (409, #"{"kind":"stale_push_preferences"}"#, .incompatible),
        (403, #"{"kind":"stale_push_preferences","current_revision":77}"#, .approvalRequired),
    ])
    func legacyAndFutureResponsesNeverUseProseForRecovery(status: Int, body: String, reason: RegistrationFailure.Reason) {
        #expect(RegistrationFailure.response(status: status, body: Data(body.utf8), operation: .mac).reason == reason)
    }

    @Test func aRelayFailureNeverClaimsTheMacRefusedRegistration() async throws {
        let transport = RegistrationResponses(status: 204, body: Data(), relayError: URLError(.timedOut))
        let api = AlondraAPIClient(serviceURL: URL(string: "https://example.test")!, deviceID: "client", operations: transport)
        do {
            try await api.registerServicePush(token: "test-token", environment: "sandbox", binding: String(repeating: "A", count: 43), preferencesRevision: 1)
            Issue.record("Relay failure was ignored")
        } catch let failure as RegistrationFailure {
            #expect(failure == RegistrationFailure(operation: .relay, reason: .unavailable))
        }
        #expect(await transport.macCalls == 0)
    }

    @Test func authenticationAndIncompatibilityNeedActionButCapacityAndEnrollmentRetry() {
        for reason in [RegistrationFailure.Reason.approvalRequired, .authenticationRequired, .incompatible, .invalidRequest(.invalidActivityKey), .local, .rejected] {
            #expect(!RegistrationFailure(operation: .mac, reason: reason).retries)
        }
        for reason in [RegistrationFailure.Reason.unavailable, .capacity, .notEnrolled, .expired, .stalePreferences(3)] {
            #expect(RegistrationFailure(operation: .mac, reason: reason).retries)
        }
    }
}

private actor RegistrationResponses: OperationAPI, PushRegistrationAPI {
    let status: Int
    let body: Data
    let relayError: (any Error)?
    var macCalls = 0
    init(status: Int, body: Data, relayError: (any Error)? = nil) { self.status = status; self.body = body; self.relayError = relayError }
    func registerServicePush(token: String, environment: String, binding: String) async throws {
        if let relayError { throw relayError }
    }
    func perform(_ request: APIRequest) async throws -> APIResponse {
        macCalls += 1
        return .completed(status: status, body: body)
    }
}
