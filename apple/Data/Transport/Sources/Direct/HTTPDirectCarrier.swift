import Foundation

/// Sealed requests over the Mac's direct HTTP port, on whichever address the router picks.
struct HTTPDirectCarrier: EnvelopeCarrier {
    let router: DirectRouter
    let http: DirectHTTP
    let pause: @Sendable (Duration) async throws -> Void

    /// Up to `attempts` tries, re-picking the route after a network failure or an impostor. A mutation that may have
    /// reached the Mac without an answer is `.unknown`, never silently dropped.
    func deliver(_ body: Data, open: @escaping @Sendable (Envelope) throws -> APIResponse, mutation: Bool,
                 timeout: Duration, attempts: Int, lifetime: DirectSessionLifetime) async throws -> CarrierAnswer {
        var mayHaveArrived = false
        var routeError = DirectRouteError.unreachable
        for attempt in 0..<attempts {
            try MutationScope.check()
            guard lifetime.isActive else { throw CancellationError() }
            if attempt > 0 { try await pause(.milliseconds(500 * attempt)) }
            try MutationScope.check()
            let endpoint: DirectEndpoint
            do { endpoint = try await router.endpoint() }
            catch let error as DirectRouteError where mayHaveArrived {
                routeError = error
                break
            }
            try MutationScope.check()
            let context = await router.requestContext()
            try MutationScope.check()
            let reply: HTTPReply
            do { reply = try await http.post(endpoint, path: "/direct/request", body: body, timeout: timeout) }
            catch let error as URLError where error.code == .cancelled { throw CancellationError() }
            catch let error as URLError {
                if error.isLocalNetworkDenied { routeError = .localNetworkDenied }
                mayHaveArrived = true
                await router.invalidate()
                continue
            }
            try MutationScope.check()
            guard lifetime.isActive else { throw CancellationError() }
            if reply.status == 200 {
                // Only an answer that opens with the pinned keys proves the address is this Mac.
                if let envelope = try? JSONDecoder().decode(Envelope.self, from: reply.body),
                   let response = try? open(envelope) {
                    await router.proved(endpoint, in: context, response: response, lifetime: lifetime)
                    return .sealed(envelope, response)
                }
                await router.reject(endpoint, in: context)
                continue
            }
            // A bare status is not signed, so it never says anything about the Mac or this
            // device: a refusal rules out only that address, and the Mac stays as not answering.
            // From an address proved on this network, a conflict or a rejection is believed.
            guard reply.status != 401, await router.isProven(endpoint, in: context) else {
                await router.reject(endpoint, in: context)
                continue
            }
            switch reply.status {
            case 409: throw SealedRequestError.operationIdentityConflict
            case 421: await router.reject(endpoint, in: context)
            case 429, 503: continue
            case 400: return .plain(.completed(status: 400, body: reply.body))
            default: await router.reject(endpoint, in: context)
            }
        }
        if mutation && mayHaveArrived { return .unknown }
        throw routeError
    }
}
