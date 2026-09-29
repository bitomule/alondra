import AlondraContracts
import AlondraDomain
import Foundation
import Testing

struct CreateRequestUseCaseTests {
    @Test
    func failedAttachmentUploadNeverSendsPartialRequest() async {
        let attachment = NewRequestAttachment(id: UUID(), name: "broken", data: Data())
        let useCase = CreateRequestUseCase(operations: operations(
            upload: { _ in throw SubmissionFailure.refused },
            create: { _, _ in Issue.record("create must not be called"); return .confirmed }
        ))
        let body = NewRequestBody(text: "Do work", project: nil, options: [:], issue: nil)
        #expect(await useCase.run(body, [attachment], "command-1") == .failed("refused"))
    }

    @Test
    func uploadsAreIncludedInOrderWithProjectIssueAndAutomaticEffort() async {
        let recorder = SentBody()
        let attachments = [
            NewRequestAttachment(id: UUID(), name: "one", data: Data([1])),
            NewRequestAttachment(id: UUID(), name: "two", data: Data([2])),
        ]
        let useCase = CreateRequestUseCase(operations: operations(
            upload: { DomainV2.AttachmentRef(id: "stored-\($0.name)", name: $0.name, contentDigest: "d", contentReference: "r", bytes: 1) },
            create: { await recorder.record($0, commandID: $1); return .confirmed }
        ))
        let body = NewRequestBody(text: "Fix it", project: "Lumo", options: [:], issue: "owner/Lumo#12")
        #expect(await useCase.run(body, attachments, "command-1") == .confirmed)
        let sent = await recorder.body
        #expect(sent?.attachments.map(\.id) == ["stored-one", "stored-two"])
        #expect(await recorder.commandID == "command-1", "The draft's command ID reaches the create command")
        #expect(sent?.project == "Lumo")
        #expect(sent?.issue == "owner/Lumo#12")
        #expect(sent?.options.isEmpty == true, "No effort override means the server's automatic default remains authoritative")
    }

    @Test
    func identityRotationDuringUploadDoesNotDispatchWithReplacementSigner() async {
        let gate = SubmissionGate()
        let scope = NewWorkSubmissionScope(
            upload: { _ in await gate.rotateToB(); return DomainV2.AttachmentRef(id: "stored-A", name: "proof", contentDigest: "d", contentReference: "r", bytes: 1) },
            isCurrent: { await gate.isA },
            create: { _, _ in await gate.recordDispatch(signer: "A"); return .confirmed }
        )
        let operations = NewWorkOperations(
            projects: { [] },
            options: { _ in throw SubmissionFailure.refused },
            issues: { _ in throw SubmissionFailure.refused },
            captureSubmissionScope: { scope }
        )
        let useCase = CreateRequestUseCase(operations: operations)
        let attachment = NewRequestAttachment(id: UUID(), name: "proof", data: Data([1]))
        let body = NewRequestBody(text: "A request", project: nil, options: [:], issue: nil)
        #expect(await useCase.run(body, [attachment], "command-1") == .cancelled)
        #expect(await gate.dispatchedSigner == nil)
    }

    @Test
    func identityRotationBetweenPreflightAndCreateIsRejectedAtScopeBoundary() async {
        let gate = SubmissionGate()
        let scope = NewWorkSubmissionScope(
            upload: { _ in throw SubmissionFailure.refused },
            isCurrent: { await gate.checkThenRotateOnFirstCall() },
            create: { _, _ in await gate.recordDispatch(signer: "A"); return .confirmed }
        )
        let operations = NewWorkOperations(
            projects: { [] },
            options: { _ in throw SubmissionFailure.refused },
            issues: { _ in throw SubmissionFailure.refused },
            captureSubmissionScope: { scope }
        )
        let body = NewRequestBody(text: "A request", project: nil, options: [:], issue: nil)
        #expect(await CreateRequestUseCase(operations: operations).run(body, [], "command-1") == .cancelled)
        #expect(await gate.dispatchedSigner == nil)
    }

    private func operations(
        options: @escaping @Sendable (String?) async throws -> OptionsCatalog = { _ in throw SubmissionFailure.refused },
        upload: @escaping @Sendable (NewRequestAttachment) async throws -> DomainV2.AttachmentRef = { _ in throw SubmissionFailure.refused },
        create: @escaping @Sendable (NewRequestBody, String) async -> CreateRequestResult = { _, _ in .confirmed }
    ) -> NewWorkOperations {
        NewWorkOperations(
            projects: { [] }, options: options,
            issues: { _ in try JSONDecoder().decode(IssueList.self, from: Data(#"{"issues":[]}"#.utf8)) },
            captureSubmissionScope: { NewWorkSubmissionScope(upload: upload, isCurrent: { true }, create: create) }
        )
    }
}

private enum SubmissionFailure: Error, LocalizedError {
    case refused
    var errorDescription: String? { "refused" }
}

private actor SentBody {
    private(set) var body: NewRequestBody?
    private(set) var commandID: String?
    func record(_ body: NewRequestBody, commandID: String) { self.body = body; self.commandID = commandID }
}

private actor SubmissionGate {
    private var revision = "A"
    private var checks = 0
    private(set) var dispatchedSigner: String?
    var isA: Bool { revision == "A" }
    func rotateToB() { revision = "B" }
    func checkThenRotateOnFirstCall() -> Bool {
        checks += 1
        let wasA = revision == "A"
        if checks == 1 { revision = "B" }
        return wasA
    }
    func recordDispatch(signer: String) { dispatchedSigner = signer }
}
