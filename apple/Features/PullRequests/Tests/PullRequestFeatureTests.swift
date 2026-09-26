import AlondraPullRequestsFeature
import AlondraShared
import AlondraCommands
import ComposableArchitecture
import Foundation
import Testing

private func makePR(_ extra: String = "", state: String = "open") throws -> PullRequest {
    let json = """
    {"id":4,"url":"https://github.com/a/b/pull/12","repo":"a/b","number":12,"session_id":"s","title":"Menus","state":"\(state)","opened_at":1\(extra)}
    """
    let decoder = JSONDecoder()
    decoder.keyDecodingStrategy = .convertFromSnakeCase
    return try decoder.decode(PullRequest.self, from: Data(json.utf8))
}

private actor SuspendedFirstReview {
    private var pending: CheckedContinuation<PRMutationOutcome, Never>?
    private var startedWaiter: CheckedContinuation<Void, Never>?
    private var started = false
    private var released = false
    private(set) var texts: [String] = []

    func submit(_ text: String) async -> PRMutationOutcome {
        texts.append(text)
        guard texts.count == 1 else { return .confirmed }
        started = true
        startedWaiter?.resume()
        startedWaiter = nil
        if released { return .confirmed }
        return await withCheckedContinuation { pending = $0 }
    }

    func waitUntilStarted() async {
        if started { return }
        await withCheckedContinuation { startedWaiter = $0 }
    }

    func release() {
        released = true
        pending?.resume(returning: .confirmed)
        pending = nil
    }
}

private actor DispatchCount {
    private(set) var value = 0
    func increment() { value += 1 }
}

@MainActor
struct PullRequestFeatureTests {
    @Test
    func cancellingLineEditorPreservesSavedCommentAndSaveCommitsWholeBuffer() async {
        let diff = "diff --git a/a.swift b/a.swift\n--- a/a.swift\n+++ b/a.swift\n@@ -0,0 +1 @@\n+let a = 1\n"
        var state = DiffReviewFeature.State(prID: 4)
        state.files = UnifiedDiff.parse(diff)
        state.loadedDiffText = diff
        state.loadedHeadSHA = String(repeating: "a", count: 40)
        state.commentsDiffText = diff
        state.commentsHeadSHA = state.loadedHeadSHA
        state.comments = ["a.swift:1": "Original comment"]
        let store = TestStore(initialState: state) { DiffReviewFeature() }
        await store.send(.editLine("a.swift:1")) {
            $0.editingAnchor = "a.swift:1"
            $0.editingText = "Original comment"
        }
        await store.send(.lineEditorChanged("Discarded edit")) { $0.editingText = "Discarded edit" }
        await store.send(.cancelLineComment) { $0.editingAnchor = nil; $0.editingText = "" }
        await store.send(.editLine("a.swift:1")) {
            $0.editingAnchor = "a.swift:1"
            $0.editingText = "Original comment"
        }
        await store.send(.lineEditorChanged("Replacement comment")) { $0.editingText = "Replacement comment" }
        await store.send(.saveEditedLine)
        await store.receive(\.saveLineComment) {
            $0.comments["a.swift:1"] = "Replacement comment"
            $0.editingAnchor = nil
            $0.editingText = ""
        }
    }

    @Test
    func mergeDialogCannotSendAfterPRCloses() async throws {
        let pr = try makePR(#","merge_methods":["merge","squash"]"#)
        let closed = try makePR(state: "closed")
        let store = TestStore(initialState: PullRequestFeature.State(pr: pr)) { PullRequestFeature() } withDependencies: {
            $0.pullRequestOperations = .init(
                diff: { _ in PRDiffSnapshot(diff: "", headSHA: String(repeating: "a", count: 40)) }, merge: { _, _ in Issue.record("merge dispatched after close"); return .confirmed },
                resolveConflicts: { _ in .obsolete }, setAside: { _ in .obsolete },
                review: { _, _, _, _, _ in .obsolete }
            )
        }
        await store.send(.chooseMergeMethod("squash")) { $0.selectedMergeMethod = "squash" }
        await store.send(.pullRequestChanged(closed)) {
            $0.pr = closed
            $0.isClosed = true
            $0.selectedMergeMethod = nil
        }
        await store.send(.confirmMerge)
        #expect(!store.state.isSending)
        await store.finish()
    }

    @Test
    func uncertainMergeBlocksDuplicateUntilExactCommandReconciles() async throws {
        let pr = try makePR()
        let command = UUID()
        let store = TestStore(initialState: PullRequestFeature.State(pr: pr)) { PullRequestFeature() } withDependencies: {
            $0.pullRequestOperations = .init(
                diff: { _ in PRDiffSnapshot(diff: "", headSHA: String(repeating: "a", count: 40)) }, merge: { _, _ in .unknown(command) },
                resolveConflicts: { _ in Issue.record("duplicate conflict action"); return .confirmed },
                setAside: { _ in Issue.record("duplicate set aside"); return .confirmed },
                review: { _, _, _, _, _ in .obsolete }
            )
        }
        await store.send(.chooseMergeMethod("squash")) { $0.selectedMergeMethod = "squash" }
        await store.send(.confirmMerge) {
            $0.selectedMergeMethod = nil
            $0.isSending = true
        }
        await store.receive(\.mutationFinished) {
            $0.isSending = false
            $0.mutationOutcome = .unknown(command)
        }
        await store.send(.setAsideTapped)
        await store.send(.commandReconciled(UUID()))
        #expect(store.state.mutationOutcome == .unknown(command))
        await store.send(.commandReconciled(command)) { $0.mutationOutcome = nil }
        await store.finish()
    }

    @Test
    func reviewKeepsDraftAndShowsClosed409() async {
        let files = UnifiedDiff.parse("""
        diff --git a/a.swift b/a.swift
        --- a/a.swift
        +++ b/a.swift
        @@ -1 +1 @@
        -old
        +new
        """)
        var state = DiffReviewFeature.State(prID: 4)
        state.files = files
        state.loadedDiffText = "original diff"
        state.loadedHeadSHA = String(repeating: "a", count: 40)
        state.commentsDiffText = "original diff"
        state.commentsHeadSHA = String(repeating: "a", count: 40)
        state.comments = ["a.swift:1": "Please rename"]
        let store = TestStore(initialState: state) { DiffReviewFeature() } withDependencies: {
            $0.pullRequestOperations = .init(
                diff: { _ in PRDiffSnapshot(diff: "original diff", headSHA: String(repeating: "a", count: 40)) }, merge: { _, _ in .obsolete }, resolveConflicts: { _ in .obsolete },
                setAside: { _ in .obsolete },
                review: { _, _, _, _, _ in .closed }
            )
        }
        await store.send(.requestChangesTapped) {
            $0.isSending = true
            $0.submittedComments = ["a.swift:1": "Please rename"]
            $0.submittedNote = ""
            $0.submittedAttachments = []
        }
        await store.receive(\.reviewFinished) {
            $0.isSending = false
            $0.mutationOutcome = .closed
            $0.validationError = "This PR is no longer open"
            $0.isOpen = false
            $0.submittedComments = nil
            $0.submittedNote = nil
            $0.submittedAttachments = nil
        }
        #expect(store.state.comments["a.swift:1"] == "Please rename")
        await store.send(.requestChangesTapped)
        await store.finish()
    }

    @Test
    func staleLineAnchorCannotSendPartialReview() {
        let files = UnifiedDiff.parse("""
        diff --git a/a.swift b/a.swift
        --- a/a.swift
        +++ b/a.swift
        @@ -1 +1 @@
        -old
        +new
        """)
        let result = PreparePRReviewUseCase().execute(
            comments: ["a.swift:99": "Old comment", "a.swift:1": "Current comment"],
            note: "", attachments: [], files: files, isOpen: true, commentsAreCurrent: true
        )
        #expect(result == .outdatedDiff)
    }

    @Test
    func oversizedDiffBlocksReviewAndKeepsGeneralNote() async {
        let store = TestStore(initialState: DiffReviewFeature.State(prID: 4)) { DiffReviewFeature() } withDependencies: {
            $0.pullRequestOperations = .init(
                diff: { _ in throw PRDiffLoadError.responseTooLarge }, merge: { _, _ in .obsolete },
                resolveConflicts: { _ in .obsolete }, setAside: { _ in .obsolete },
                review: { _, _, _, _, _ in Issue.record("review sent without verified diff"); return .confirmed }
            )
        }
        await store.send(.appeared) { $0.isLoading = true; $0.loadGeneration = 1 }
        await store.receive(\.diffFailed) {
            $0.isLoading = false
            $0.loadError = "This diff is too large to review on this device"
        }
        await store.send(.noteChanged("Please summarize the large diff")) { $0.note = "Please summarize the large diff" }
        await store.send(.askTapped)
        #expect(store.state.note == "Please summarize the large diff")
        await store.finish()
    }

    @Test
    func lateAcknowledgementPreservesEditsMadeDuringReviewSend() async {
        var state = DiffReviewFeature.State(prID: 4)
        state.isSending = true
        state.note = "New follow-up note"
        state.submittedNote = "Original note"
        state.submittedComments = [:]
        state.submittedAttachments = []
        let store = TestStore(initialState: state) { DiffReviewFeature() }
        await store.send(.reviewFinished(.confirmed, 0)) {
            $0.isSending = false
            $0.successfulActionCount = 1
            $0.submittedNote = nil
            $0.submittedComments = nil
            $0.submittedAttachments = nil
        }
        #expect(store.state.note == "New follow-up note")
        await store.finish()
    }

    @Test
    func oldDiffResultCannotReplaceNewOpenRequest() async {
        let store = TestStore(initialState: DiffReviewFeature.State(prID: 4)) { DiffReviewFeature() } withDependencies: {
            $0.pullRequestOperations = .init(
                diff: { _ in throw CancellationError() }, merge: { _, _ in .obsolete },
                resolveConflicts: { _ in .obsolete }, setAside: { _ in .obsolete },
                review: { _, _, _, _, _ in .obsolete }
            )
        }
        await store.send(.appeared) { $0.isLoading = true; $0.loadGeneration = 1 }
        await store.send(.disappeared) { $0.isLoading = false; $0.loadGeneration = 2 }
        await store.send(.appeared) { $0.isLoading = true; $0.loadGeneration = 3 }
        await store.send(.diffLoaded([], PRDiffSnapshot(diff: "stale", headSHA: String(repeating: "a", count: 40)), 1))
        #expect(store.state.isLoading && store.state.files == nil)
        await store.send(.diffLoaded([], PRDiffSnapshot(diff: "current", headSHA: String(repeating: "a", count: 40)), 3)) {
            $0.isLoading = false
            $0.files = []
            $0.loadedDiffText = "current"
            $0.loadedHeadSHA = String(repeating: "a", count: 40)
        }
        await store.finish()
    }

    @Test
    func sameAnchorOnChangedDiffRequiresExplicitReconfirmation() async {
        let oldText = """
        diff --git a/a.swift b/a.swift
        --- a/a.swift
        +++ b/a.swift
        @@ -1 +1 @@
        -old
        +first
        """
        let newText = oldText.replacingOccurrences(of: "+first", with: "+second")
        var state = DiffReviewFeature.State(prID: 4)
        state.files = UnifiedDiff.parse(oldText)
        state.loadedDiffText = oldText
        state.loadedHeadSHA = String(repeating: "a", count: 40)
        state.commentsDiffText = oldText
        state.commentsHeadSHA = String(repeating: "a", count: 40)
        state.comments = ["a.swift:1": "Change this"]
        state.isLoading = true
        state.loadGeneration = 1
        let store = TestStore(initialState: state) { DiffReviewFeature() }
        await store.send(.diffLoaded(UnifiedDiff.parse(newText), PRDiffSnapshot(diff: newText, headSHA: String(repeating: "a", count: 40)), 1)) {
            $0.isLoading = false
            $0.files = UnifiedDiff.parse(newText)
            $0.loadedDiffText = newText
            $0.loadedHeadSHA = String(repeating: "a", count: 40)
        }
        #expect(store.state.commentsNeedReview)
        await store.send(.requestChangesTapped) { $0.validationError = "The diff changed; refresh before sending line comments" }
        await store.send(.confirmCommentsForCurrentDiff) {
            $0.commentsDiffText = newText
            $0.commentsHeadSHA = String(repeating: "a", count: 40)
            $0.validationError = nil
        }
        #expect(!store.state.commentsNeedReview)
        await store.finish()
    }

    @Test
    func attachmentsWithoutTextAreRejectedBeforeTransport() {
        let attachment = PRReviewAttachment(id: UUID(), name: "log.txt", data: Data("log".utf8))
        let decision = PreparePRReviewUseCase().execute(
            comments: [:], note: "  ", attachments: [attachment], files: nil,
            isOpen: true, commentsAreCurrent: true
        )
        #expect(decision == .textRequired)
    }

    @Test
    func removedLineCommentCanBeDiscardedAfterDiffChanges() async {
        var state = DiffReviewFeature.State(prID: 4)
        state.comments = ["a.swift:99": "No longer exists"]
        state.commentsDiffText = "old diff"
        state.commentsHeadSHA = String(repeating: "a", count: 40)
        state.loadedDiffText = "new diff"
        state.loadedHeadSHA = String(repeating: "a", count: 40)
        let store = TestStore(initialState: state) { DiffReviewFeature() }
        await store.send(.removeLineComment("a.swift:99")) {
            $0.comments = [:]
            $0.commentsDiffText = nil
            $0.commentsHeadSHA = nil
        }
        #expect(!store.state.commentsNeedReview)
        await store.finish()
    }

    @Test
    func commentAddedDuringSuspendedSendIsRetriedWithoutResendingFirstComment() async {
        let diff = """
        diff --git a/a.swift b/a.swift
        --- a/a.swift
        +++ b/a.swift
        @@ -1,2 +1,2 @@
        -old A
        -old B
        +new A
        +new B
        """
        var state = DiffReviewFeature.State(prID: 4)
        state.files = UnifiedDiff.parse(diff)
        state.loadedDiffText = diff
        state.loadedHeadSHA = String(repeating: "a", count: 40)
        state.commentsDiffText = diff
        state.commentsHeadSHA = String(repeating: "a", count: 40)
        state.comments = ["a.swift:1": "Fix A"]
        let gate = SuspendedFirstReview()
        let store = TestStore(initialState: state) { DiffReviewFeature() } withDependencies: {
            $0.pullRequestOperations = .init(
                diff: { _ in PRDiffSnapshot(diff: diff, headSHA: String(repeating: "a", count: 40)) }, merge: { _, _ in .obsolete }, resolveConflicts: { _ in .obsolete },
                setAside: { _ in .obsolete },
                review: { _, text, _, _, headSHA in
                    #expect(headSHA == String(repeating: "a", count: 40))
                    return await gate.submit(text)
                }
            )
        }
        await store.send(.requestChangesTapped) {
            $0.isSending = true
            $0.submittedComments = ["a.swift:1": "Fix A"]
            $0.submittedNote = ""
            $0.submittedAttachments = []
        }
        await gate.waitUntilStarted()
        await store.send(.editLine("a.swift:2")) { $0.editingAnchor = "a.swift:2" }
        await store.send(.saveLineComment("a.swift:2", "Fix B")) {
            $0.editingAnchor = nil
            $0.comments["a.swift:2"] = "Fix B"
        }
        await gate.release()
        await store.receive(\.reviewFinished) {
            $0.isSending = false
            $0.comments = ["a.swift:2": "Fix B"]
            $0.successfulActionCount = 1
            $0.submittedComments = nil
            $0.submittedNote = nil
            $0.submittedAttachments = nil
        }
        await store.send(.requestChangesTapped) {
            $0.isSending = true
            $0.submittedComments = ["a.swift:2": "Fix B"]
            $0.submittedNote = ""
            $0.submittedAttachments = []
        }
        await store.receive(\.reviewFinished) {
            $0.isSending = false
            $0.comments = [:]
            $0.commentsDiffText = nil
            $0.commentsHeadSHA = nil
            $0.mutationOutcome = .confirmed
            $0.shouldDismiss = true
            $0.successfulActionCount = 2
            $0.submittedComments = nil
            $0.submittedNote = nil
            $0.submittedAttachments = nil
        }
        let calls = await gate.texts
        #expect(calls.count == 2)
        #expect(calls[0].contains("Fix A") && !calls[0].contains("Fix B"))
        #expect(calls[1].contains("Fix B") && !calls[1].contains("Fix A"))
        await store.finish()
    }

    @Test
    func pushedDiffWhileScreenOpenStopsReviewBeforeMutation() async {
        let first = """
        diff --git a/a.swift b/a.swift
        --- a/a.swift
        +++ b/a.swift
        @@ -1 +1 @@
        -old
        +first
        """
        let latest = first.replacingOccurrences(of: "+first", with: "+second")
        var state = DiffReviewFeature.State(prID: 4)
        state.files = UnifiedDiff.parse(first)
        state.loadedDiffText = first
        state.loadedHeadSHA = String(repeating: "a", count: 40)
        state.commentsDiffText = first
        state.commentsHeadSHA = String(repeating: "a", count: 40)
        state.comments = ["a.swift:1": "Fix this"]
        let store = TestStore(initialState: state) { DiffReviewFeature() } withDependencies: {
            $0.pullRequestOperations = .init(
                diff: { _ in PRDiffSnapshot(diff: latest, headSHA: String(repeating: "a", count: 40)) }, merge: { _, _ in .obsolete }, resolveConflicts: { _ in .obsolete },
                setAside: { _ in .obsolete },
                review: { _, _, _, _, _ in Issue.record("stale diff review sent"); return .confirmed }
            )
        }
        await store.send(.requestChangesTapped) {
            $0.isSending = true
            $0.submittedComments = ["a.swift:1": "Fix this"]
            $0.submittedNote = ""
            $0.submittedAttachments = []
        }
        await store.receive(\.reviewDiffChanged) {
            $0.isSending = false
            $0.loadGeneration = 1
            $0.files = UnifiedDiff.parse(latest)
            $0.loadedDiffText = latest
            $0.loadedHeadSHA = String(repeating: "a", count: 40)
            $0.validationError = "The PR changed; review the new diff before sending"
            $0.submittedComments = nil
            $0.submittedNote = nil
            $0.submittedAttachments = nil
        }
        #expect(store.state.commentsNeedReview)
        await store.finish()
    }

    @Test
    func changedHeadWithIdenticalDiffStopsReviewBeforeMutation() async {
        let first = """
        diff --git a/a.swift b/a.swift
        --- a/a.swift
        +++ b/a.swift
        @@ -1 +1 @@
        -old
        +first
        """
        let latest = first
        var state = DiffReviewFeature.State(prID: 4)
        state.files = UnifiedDiff.parse(first)
        state.loadedDiffText = first
        state.loadedHeadSHA = String(repeating: "a", count: 40)
        state.commentsDiffText = first
        state.commentsHeadSHA = String(repeating: "a", count: 40)
        state.comments = ["a.swift:1": "Fix this"]
        let store = TestStore(initialState: state) { DiffReviewFeature() } withDependencies: {
            $0.pullRequestOperations = .init(
                diff: { _ in PRDiffSnapshot(diff: latest, headSHA: String(repeating: "b", count: 40)) }, merge: { _, _ in .obsolete }, resolveConflicts: { _ in .obsolete },
                setAside: { _ in .obsolete },
                review: { _, _, _, _, _ in Issue.record("stale diff review sent"); return .confirmed }
            )
        }
        await store.send(.requestChangesTapped) {
            $0.isSending = true
            $0.submittedComments = ["a.swift:1": "Fix this"]
            $0.submittedNote = ""
            $0.submittedAttachments = []
        }
        await store.receive(\.reviewDiffChanged) {
            $0.isSending = false
            $0.loadGeneration = 1
            $0.files = UnifiedDiff.parse(latest)
            $0.loadedDiffText = latest
            $0.loadedHeadSHA = String(repeating: "b", count: 40)
            $0.validationError = "The PR changed; review the new diff before sending"
            $0.submittedComments = nil
            $0.submittedNote = nil
            $0.submittedAttachments = nil
        }
        #expect(store.state.commentsNeedReview)
        await store.finish()
    }

    @Test
    func failedPreflightDoesNotReserveOrRetryMutation() async {
        enum Offline: LocalizedError {
            case unavailable
            var errorDescription: String? { "offline" }
        }
        var state = DiffReviewFeature.State(prID: 4)
        state.files = []
        state.loadedDiffText = "known diff"
        state.loadedHeadSHA = String(repeating: "a", count: 40)
        state.note = "Please check"
        let store = TestStore(initialState: state) { DiffReviewFeature() } withDependencies: {
            $0.pullRequestOperations = .init(
                diff: { _ in throw Offline.unavailable }, merge: { _, _ in .obsolete },
                resolveConflicts: { _ in .obsolete }, setAside: { _ in .obsolete },
                review: { _, _, _, _, _ in Issue.record("mutation sent after failed preflight"); return .confirmed }
            )
        }
        await store.send(.askTapped) {
            $0.isSending = true
            $0.submittedComments = [:]
            $0.submittedNote = "Please check"
            $0.submittedAttachments = []
        }
        await store.receive(\.reviewPreflightFailed) {
            $0.isSending = false
            $0.validationError = "Could not verify the latest diff; review was not sent: offline"
            $0.submittedComments = nil
            $0.submittedNote = nil
            $0.submittedAttachments = nil
        }
        #expect(store.state.note == "Please check")
        await store.finish()
    }

    @Test
    func oversizedResponseAfterPostIsJournaledUnknownAndCannotSendTwice() async throws {
        let directory = FileManager.default.temporaryDirectory
            .appendingPathComponent("pr-review-journal-\(UUID().uuidString)", isDirectory: true)
        try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true)
        defer { try? FileManager.default.removeItem(at: directory) }
        let scope = CommandScope(
            server: URL(string: "https://relay.example")!, deviceID: "phone",
            credentialRevision: "key-1", generation: 0
        )
        let gate = CommandGate(scope: scope, journal: FileCommandJournal(directory: directory))
        let dispatches = DispatchCount()
        let first = await gate.submit(
            .requestChanges(4), itemID: "p4", in: scope,
            isUnknown: PRPostFailureClassifier.isUnknown
        ) {
            await dispatches.increment()
            throw PRPostDispatchUncertain()
        }
        guard case .unknown(let pending) = first else {
            Issue.record("POST 502 after dispatch was treated as rejected")
            return
        }
        let relaunched = CommandGate(scope: scope, journal: FileCommandJournal(directory: directory))
        #expect(try await relaunched.restore() == [pending])
        let second = await relaunched.submit(
            .requestChanges(4), itemID: "p4", in: scope,
            isUnknown: PRPostFailureClassifier.isUnknown
        ) {
            await dispatches.increment()
        }
        guard case .unknown(let same) = second else {
            Issue.record("unknown POST was retried")
            return
        }
        #expect(same.id == pending.id)
        #expect(await dispatches.value == 1)
        #expect(await relaunched.uncertain == [pending])
    }

    @Test
    func refreshedDiffCannotBeReplacedByOlderLoadAndLoadingBlocksSend() async {
        let old = """
        diff --git a/a.swift b/a.swift
        --- a/a.swift
        +++ b/a.swift
        @@ -1 +1 @@
        -old
        +first
        """
        let latest = old.replacingOccurrences(of: "+first", with: "+second")
        var state = DiffReviewFeature.State(prID: 4)
        state.files = UnifiedDiff.parse(old)
        state.loadedDiffText = old
        state.loadedHeadSHA = String(repeating: "a", count: 40)
        state.commentsDiffText = old
        state.commentsHeadSHA = String(repeating: "a", count: 40)
        state.comments = ["a.swift:1": "Fix"]
        state.isSending = true
        state.isLoading = true
        state.loadGeneration = 7
        let store = TestStore(initialState: state) { DiffReviewFeature() }
        await store.send(.reviewDiffChanged(UnifiedDiff.parse(latest), PRDiffSnapshot(diff: latest, headSHA: String(repeating: "a", count: 40)), 0)) {
            $0.isSending = false
            $0.isLoading = false
            $0.loadGeneration = 8
            $0.files = UnifiedDiff.parse(latest)
            $0.loadedDiffText = latest
            $0.loadedHeadSHA = String(repeating: "a", count: 40)
            $0.validationError = "The PR changed; review the new diff before sending"
        }
        await store.send(.diffLoaded(UnifiedDiff.parse(old), PRDiffSnapshot(diff: old, headSHA: String(repeating: "a", count: 40)), 7))
        #expect(store.state.loadedDiffText == latest && store.state.commentsNeedReview)
        await store.finish()

        var loading = DiffReviewFeature.State(prID: 4)
        loading.files = UnifiedDiff.parse(old)
        loading.loadedDiffText = old
        loading.note = "Review note"
        loading.isLoading = true
        let loadingStore = TestStore(initialState: loading) { DiffReviewFeature() }
        await loadingStore.send(.askTapped)
        #expect(!loadingStore.state.isSending)
        await loadingStore.finish()
    }
}
