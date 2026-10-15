import AlondraContract
import AlondraFeatures
import AlondraTransport
import ComposableArchitecture
import XCTest

@MainActor
final class MacSettingsFeatureTests: XCTestCase {
    private let projects = MacSettings.Candidate(path: "/Users/d/Projects", repos: 63)
    private let developer = MacSettings.Candidate(path: "/Users/d/Developer", repos: 4)
    private let code = MacSettings.Candidate(path: "/Users/d/Code", repos: 2)

    private func settings(_ root: MacSettings.Candidate, relay: Bool = true) -> MacSettings {
        let others = [projects, developer, code].filter { $0 != root }
        return MacSettings(projectsRoot: root.path, candidates: [root] + others, relay: .init(on: relay))
    }

    private func store(save: @escaping @Sendable (String) async throws -> MacSettings,
                       load: (@Sendable () async throws -> MacSettings)? = nil,
                       folders: @escaping @Sendable (String?) async throws -> FolderListing = { _ in throw CancellationError() },
                       pickLocalFolder: (@Sendable () async -> String?)? = nil) -> TestStoreOf<MacSettingsFeature> {
        let initial = settings(projects)
        let client = MacSettingsClient(load: load ?? { initial }, save: save, folders: folders, pickLocalFolder: pickLocalFolder)
        let store = TestStore(initialState: MacSettingsFeature.State(id: UUID(), macName: "MacBook Pro", client: client)) {
            MacSettingsFeature()
        }
        store.exhaustivity = .off
        return store
    }

    func testReadsTheFolderItsCandidatesAndTheRelay() async throws {
        let json = #"""
        {"projects_root":"/Users/d/Projects","candidates":[{"path":"/Users/d/Projects","repos":63},{"path":"/Users/d/Backups","repos":1}],
         "relay":{"on":false},"error":null}
        """#
        let decoder = JSONDecoder()
        decoder.keyDecodingStrategy = .convertFromSnakeCase
        let decoded = try decoder.decode(MacSettings.self, from: Data(json.utf8))
        XCTAssertEqual(decoded, MacSettings(projectsRoot: "/Users/d/Projects",
                                            candidates: [.init(path: "/Users/d/Projects", repos: 63), .init(path: "/Users/d/Backups", repos: 1)],
                                            relay: .init(on: false)))

        let store = store(save: { _ in throw CancellationError() }, load: { decoded })
        await store.send(.task)
        await store.receive(\.loaded)
        XCTAssertEqual(store.state.projectsRoot, "/Users/d/Projects")
        XCTAssertEqual(store.state.settings?.relay.on, false)
    }

    func testTheOpenScreenReadsTheSettingsAgainSoTheMacsDiagnosisStaysCurrent() async {
        let denied = DirectConnection(verdict: .relayOnly, reasons: [.localNetworkDenied])
        let allowed = DirectConnection(verdict: .reachable)
        let reads = LockIsolated(0)
        let client = MacSettingsClient(load: {
            let read = reads.withValue { $0 += 1; return $0 }
            return MacSettings(projectsRoot: "/p", candidates: [], relay: .init(on: true), directConnection: read == 1 ? denied : allowed)
        }, save: { _ in throw CancellationError() }, folders: { _ in throw CancellationError() }, refresh: .milliseconds(10))
        let store = TestStore(initialState: MacSettingsFeature.State(id: UUID(), macName: "Mac", client: client)) { MacSettingsFeature() }
        store.exhaustivity = .off
        let task = await store.send(.task)
        await store.receive(\.loaded)
        XCTAssertEqual(store.state.settings?.directConnection, denied)
        await store.receive(\.loaded)
        XCTAssertEqual(store.state.settings?.directConnection, allowed)
        await task.cancel()
    }

    func testAMacThatDoesNotAnswerShowsWhy() async {
        let client = MacSettingsClient(load: { throw URLError(.timedOut) }, save: { _ in throw CancellationError() },
                                       folders: { _ in throw CancellationError() }, problem: { _ in .macNotAnswering })
        let store = TestStore(initialState: MacSettingsFeature.State(id: UUID(), macName: "Mac", client: client)) { MacSettingsFeature() }
        store.exhaustivity = .off
        await store.send(.task)
        await store.receive(\.loadFailed)
        XCTAssertEqual(store.state.problem, .macNotAnswering)
    }

    func testAChosenFolderShowsAtOnceAndKeepsTheMacsAnswer() async {
        let gate = Gate()
        let saved = LockIsolated<[String]>([])
        let answer = settings(code)
        let store = store(save: { path in
            saved.withValue { $0.append(path) }
            await gate.wait()
            return answer
        })
        await store.send(.loaded(settings(projects)))
        await store.send(.choose(code))
        XCTAssertEqual(store.state.projectsRoot, code.path, "it shows before the Mac answers")
        await gate.open()
        await store.receive(\.saved)
        XCTAssertEqual(store.state.projectsRoot, code.path)
        XCTAssertEqual(store.state.settings?.candidates.first, code)
        XCTAssertNil(store.state.error)
        XCTAssertEqual(saved.value, [code.path])
    }

    func testARefusedFolderGoesBackWithTheMacsReasonAndRereadsTheMac() async {
        let loads = LockIsolated(0)
        let initial = settings(projects)
        let store = store(save: { _ in
            throw APIError.server(status: 422, kind: "not_a_directory", detail: "~/notas es un archivo, no una carpeta.", reasons: [])
        }, load: {
            loads.withValue { $0 += 1 }
            return initial
        })
        await store.send(.loaded(initial))
        await store.send(.choose(code))
        await store.receive(\.refused)
        XCTAssertEqual(store.state.projectsRoot, projects.path)
        XCTAssertEqual(store.state.error, "~/notas es un archivo, no una carpeta.")
        await store.receive(\.loaded)
        XCTAssertEqual(loads.value, 1, "the Mac says which folder it really kept")
    }

    func testAChangeThatNeverReachedTheMacGoesBackWithAGenericReason() async {
        let store = store(save: { _ in throw URLError(.notConnectedToInternet) })
        await store.send(.loaded(settings(projects)))
        await store.send(.choose(developer))
        await store.receive(\.refused)
        XCTAssertEqual(store.state.projectsRoot, projects.path)
        XCTAssertEqual(store.state.error, String(localized: "Your Mac didn’t get the new folder. Try again."))
    }

    func testALateAnswerNeverOverwritesANewerChoice() async {
        let first = Gate()
        let late = settings(developer), latest = settings(code), slow = developer.path
        let store = store(save: { path in
            if path == slow {
                await first.wait()
                return late
            }
            return latest
        })
        await store.send(.loaded(settings(projects)))
        await store.send(.choose(developer))
        await store.send(.choose(code))
        await store.receive(\.saved)
        XCTAssertEqual(store.state.projectsRoot, code.path)
        await first.open()
        await store.receive(\.saved)
        XCTAssertEqual(store.state.projectsRoot, code.path, "the last choice wins")
        XCTAssertEqual(store.state.settings?.projectsRoot, code.path)
    }

    func testOtherWalksTheMacsFoldersAndSavesTheOneShown() async {
        let home = FolderListing(path: "/Users/d", parent: nil, folders: [
            .init(name: "Work", path: "/Users/d/Work", repo: false, repos: 3),
        ])
        let work = FolderListing(path: "/Users/d/Work", parent: "/Users/d", folders: [
            .init(name: "app", path: "/Users/d/Work/app", repo: true, repos: 0),
            .init(name: "site", path: "/Users/d/Work/site", repo: true, repos: 0),
            .init(name: "notes", path: "/Users/d/Work/notes", repo: false, repos: 0),
        ])
        let asked = LockIsolated<[String?]>([])
        let saved = LockIsolated<[String]>([])
        let store = store(save: { path in
            saved.withValue { $0.append(path) }
            return MacSettings(projectsRoot: path, candidates: [.init(path: path, repos: 2)], relay: .init(on: true))
        }, folders: { path in
            asked.withValue { $0.append(path) }
            return path == nil || path == "/Users/d" ? home : work
        })
        await store.send(.loaded(settings(projects)))
        await store.send(.other)
        XCTAssertNotNil(store.state.browser)
        await store.send(\.browser.task)
        await store.receive(\.browser.loaded)
        await store.send(\.browser.open, "/Users/d/Work")
        await store.receive(\.browser.loaded)
        XCTAssertEqual(store.state.browser?.listing, work)
        await store.send(\.browser.open, work.parent)
        await store.receive(\.browser.loaded)
        await store.send(\.browser.open, "/Users/d/Work")
        await store.receive(\.browser.loaded)
        await store.send(\.browser.use)
        await store.receive(\.browser.delegate)
        XCTAssertNil(store.state.browser)
        await store.receive(\.choose)
        XCTAssertEqual(store.state.chosen, .init(path: "/Users/d/Work", repos: 2), "the two repos inside show at once")
        await store.receive(\.saved)
        XCTAssertEqual(asked.value, [nil, "/Users/d/Work", "/Users/d", "/Users/d/Work"])
        XCTAssertEqual(saved.value, ["/Users/d/Work"])
    }

    func testOtherOnThisMacUsesMacOSsFolderPanel() async {
        let picks = LockIsolated<[String?]>(["/Users/d/Elsewhere", nil])
        let saved = LockIsolated<[String]>([])
        let store = store(save: { path in
            saved.withValue { $0.append(path) }
            return MacSettings(projectsRoot: path, candidates: [.init(path: path, repos: 0)], relay: .init(on: true))
        }, pickLocalFolder: { picks.withValue { $0.removeFirst() } })
        await store.send(.loaded(settings(projects)))
        await store.send(.other)
        XCTAssertNil(store.state.browser)
        await store.receive(\.pickedLocalFolder)
        await store.receive(\.saved)
        await store.send(.other)
        await store.receive(\.pickedLocalFolder)
        XCTAssertEqual(saved.value, ["/Users/d/Elsewhere"], "closing the panel changes nothing")
    }

    func testFoldersReadTheWayTheMacsOwnerWritesThem() {
        XCTAssertEqual(FolderPath.display("/Users/d/Projects"), "~/Projects")
        XCTAssertEqual(FolderPath.display("/Users/d"), "~")
        XCTAssertEqual(FolderPath.display("/Volumes/Work/Code"), "/Volumes/Work/Code")
    }
}
