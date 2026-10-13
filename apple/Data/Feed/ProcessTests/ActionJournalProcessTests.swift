import AlondraContract
import AlondraTestSupport
import Darwin
import Foundation
import Testing
import Synchronization

private enum ProcessFixtureError: Error {
    case missingRunfile, invalidLine, childFailed
    case endOfOutput(String), deadline(String)
}

private final class FixtureProcess: Sendable {
    private struct Lifecycle { var status: Int32? }
    private struct ReadState { var expired = false; var finished = false }
    let pid: Int32
    private let lifecycle = Mutex(Lifecycle())
    private let input: FileHandle
    private let output: FileHandle
    private let errorOutput: FileHandle
    private let reader = DispatchQueue(label: "alondra.process-fixture-reader")
    private let watchdog = DispatchQueue(label: "alondra.process-fixture-watchdog")
    private let prefix: String

    init(executable: URL, arguments: [String], home: URL, prefix: String, additionalEnvironment: [String: String] = [:]) throws {
        let stdin = Pipe(), stdout = Pipe()
        input = stdin.fileHandleForWriting; output = stdout.fileHandleForReading
        self.prefix = prefix
        let errors = home.appending(path: "process-errors.log")
        guard FileManager.default.createFile(atPath: errors.path, contents: nil) else { throw ProcessFixtureError.childFailed }
        errorOutput = try FileHandle(forWritingTo: errors)
        var environment = ProcessInfo.processInfo.environment
        environment["HOME"] = home.path; environment["TEST_TMPDIR"] = home.path; environment["TMPDIR"] = home.path
        environment["PWD"] = home.path
        environment.merge(additionalEnvironment) { _, value in value }
        for handle in [input, output, errorOutput, stdin.fileHandleForReading, stdout.fileHandleForWriting] {
            guard fcntl(handle.fileDescriptor, F_SETFD, FD_CLOEXEC) == 0 else { throw ProcessFixtureError.childFailed }
        }
        let argv = ([executable.path] + arguments).map { strdup($0) }
        let envp = environment.map { strdup($0.key + "=" + $0.value) }
        defer { for value in argv + envp { free(value) } }
        var actions: posix_spawn_file_actions_t?
        guard posix_spawn_file_actions_init(&actions) == 0 else { throw ProcessFixtureError.childFailed }
        defer { posix_spawn_file_actions_destroy(&actions) }
        guard posix_spawn_file_actions_adddup2(&actions, stdin.fileHandleForReading.fileDescriptor, STDIN_FILENO) == 0,
              posix_spawn_file_actions_adddup2(&actions, stdout.fileHandleForWriting.fileDescriptor, STDOUT_FILENO) == 0,
              posix_spawn_file_actions_adddup2(&actions, errorOutput.fileDescriptor, STDERR_FILENO) == 0,
              posix_spawn_file_actions_addclose(&actions, input.fileDescriptor) == 0,
              posix_spawn_file_actions_addclose(&actions, output.fileDescriptor) == 0 else { throw ProcessFixtureError.childFailed }
        var child: pid_t = 0
        var terminatedArgv = argv + [nil], terminatedEnvp = envp + [nil]
        let result = terminatedArgv.withUnsafeMutableBufferPointer { args in
            terminatedEnvp.withUnsafeMutableBufferPointer { env in
                posix_spawn(&child, executable.path, &actions, nil, args.baseAddress!, env.baseAddress!)
            }
        }
        guard result == 0, child > 0 else { throw ProcessFixtureError.childFailed }
        pid = child
        try stdin.fileHandleForReading.close(); try stdout.fileHandleForWriting.close()
        print("fixture pid=\(pid) started \(executable.lastPathComponent)")
    }
    func send<T: Encodable>(_ command: T) throws {
        var bytes = try JSONEncoder().encode(command); bytes.append(10)
        let kind = (try? JSONSerialization.jsonObject(with: bytes)) as? [String: String]
        print("fixture pid=\(pid) send \(kind?["command"] ?? String(describing: T.self))")
        try input.write(contentsOf: bytes)
    }
    func next<T: Decodable & Sendable>(_ type: T.Type, phase: String = #function) async throws -> T {
        let state = Mutex(ReadState())
        let timer = DispatchSource.makeTimerSource(queue: watchdog)
        timer.schedule(deadline: .now() + 45)
        timer.setEventHandler {
            let kill = state.withLock { value -> Bool in
                if value.finished { return false }
                value.expired = true; return true
            }
            if kill { self.signal() }
        }
        timer.resume()
        defer { timer.cancel() }
        print("fixture pid=\(pid) awaiting \(phase) \(prefix)")
        let data: Data = try await withTaskCancellationHandler {
            try await withCheckedThrowingContinuation { continuation in
                reader.async {
                    do {
                        while true {
                            var bytes = Data(), sawByte = false
                            while let byte = try self.output.read(upToCount: 1), !byte.isEmpty {
                                sawByte = true
                                if byte[0] == 10 { break }
                                bytes.append(byte)
                                guard bytes.count <= 1024 * 1024 else { throw ProcessFixtureError.invalidLine }
                            }
                            guard sawByte else {
                                if state.withLock({ $0.expired }) { throw ProcessFixtureError.deadline(phase) }
                                throw ProcessFixtureError.endOfOutput(phase)
                            }
                            if bytes.isEmpty { continue }
                            guard let text = String(data: bytes, encoding: .utf8), let start = text.range(of: self.prefix) else { continue }
                            let expired = state.withLock { value in value.finished = true; return value.expired }
                            if expired { throw ProcessFixtureError.deadline(phase) }
                            continuation.resume(returning: Data(text[start.upperBound...].utf8)); return
                        }
                    } catch { continuation.resume(throwing: error) }
                }
            }
        } onCancel: { self.signal() }
        try Task.checkCancellation()
        let event = try JSONDecoder().decode(type, from: data)
        let object = (try? JSONSerialization.jsonObject(with: data)) as? [String: Any]
        print("fixture pid=\(pid) received \(object?["event"] as? String ?? "unknown")")
        return event
    }
    private func signal() {
        lifecycle.withLock { value in
            guard value.status == nil else { return }
            var status: Int32 = 0
            let exited = waitpid(pid, &status, WNOHANG)
            if exited == pid { value.status = status; return }
            if exited == 0 { _ = Darwin.kill(pid, SIGKILL) }
        }
    }
    @discardableResult func killAndWait() -> Int32 {
        lifecycle.withLock { value in
            if let status = value.status { return status & 0x7f }
            var status: Int32 = 0
            let exited = waitpid(pid, &status, WNOHANG)
            if exited == pid { value.status = status; return status & 0x7f }
            guard exited == 0 else { value.status = status; return status & 0x7f }
            _ = Darwin.kill(pid, SIGKILL)
            while waitpid(pid, &status, 0) < 0 {
                if errno != EINTR { value.status = status; return status & 0x7f }
            }
            value.status = status
            print("fixture pid=\(pid) reaped status=\(status)")
            return status & 0x7f
        }
    }
    func cleanup() {
        _ = killAndWait()
        try? input.close(); try? output.close(); try? errorOutput.close()
    }
}
private struct ServerCommand: Encodable {
    let command: String
    var requestID: String? = nil
    enum CodingKeys: String, CodingKey { case command; case requestID = "request_id" }
}
private struct ServerEvent: Decodable, Sendable {
    let event: String
    var baseURL: String?
    var dataEpoch: String?
    var actionID: String?
    var optionID: String?
    var fixtureHome: String?
    var deviceID: String?
    var signingSeed: String?
    var commandID: String?
    var effects: Int?
    var actionPosts: Int?
    var resultGets: Int?
    var acknowledgments: Int?
    var journalRecords: Int?
    enum CodingKeys: String, CodingKey {
        case event, effects, acknowledgments
        case baseURL = "base_url", dataEpoch = "data_epoch", actionID = "action_id"
        case fixtureHome = "fixture_home", optionID = "option_id", deviceID = "device_id", signingSeed = "signing_seed_base64", commandID = "command_id"
        case actionPosts = "action_posts", resultGets = "result_gets", journalRecords = "journal_records"
    }
}

private struct RelayServerEvent: Decodable, Sendable {
    let event: String
    var baseURL: String?
    var bearer: String?
    var expiredBearer: String?
    var macID: String?
    var deviceID: String?
    var signingSeed: String?
    var agreementSeed: String?
    var macSigningSeed: String?
    var macAgreementSeed: String?
    var commandID: String?
    var actionPosts: Int?
    var readPosts: Int?
    var bootstrapGets: Int?
    var pending: Int?
    var pendingNoise: Int?
    var cachedPayloadBytes: Int?
    var reservedResponseBytes: Int?
    var admittedByteCharge: Int?
    var dispatchHistory: [String]?
    enum CodingKeys: String, CodingKey {
        case event, bearer, pending
        case pendingNoise = "pending_noise", cachedPayloadBytes = "cached_payload_bytes"
        case reservedResponseBytes = "reserved_response_bytes", admittedByteCharge = "admitted_byte_charge"
        case dispatchHistory = "dispatch_history"
        case expiredBearer = "expired_bearer"
        case baseURL = "base_url", macID = "mac_id", deviceID = "device_id"
        case signingSeed = "signing_seed_base64", agreementSeed = "agreement_seed_base64"
        case macSigningSeed = "mac_signing_seed_base64", macAgreementSeed = "mac_agreement_seed_base64"
        case commandID = "command_id", actionPosts = "action_posts", readPosts = "read_posts"
        case bootstrapGets = "bootstrap_gets"
    }
}

@Suite(.temporaryFiles, .serialized, .timeLimit(.minutes(2))) struct ActionJournalProcessTests {
    private func executable(_ relative: String) throws -> URL {
        let env = ProcessInfo.processInfo.environment
        guard let root = env["TEST_SRCDIR"] else { throw ProcessFixtureError.missingRunfile }
        let result = URL(fileURLWithPath: root).appending(path: env["TEST_WORKSPACE"] ?? "_main").appending(path: relative)
        guard FileManager.default.isExecutableFile(atPath: result.path) else { throw ProcessFixtureError.missingRunfile }
        return result
    }
    private func launchClient(home: URL, config: URL) throws -> FixtureProcess {
        try FixtureProcess(executable: executable("apple/Data/Feed/ActionJournalProcessClient"),
            arguments: [home.path, config.path], home: home, prefix: "ALONDRA_CLIENT ")
    }
    private func stats(_ server: FixtureProcess) async throws -> ServerEvent {
        try server.send(ServerCommand(command: "stats"))
        let result = try await server.next(ServerEvent.self)
        #expect(result.event == "stats")
        return result
    }
    private func launchRelay(home: URL, port: Int? = nil) throws -> FixtureProcess {
        try FixtureProcess(executable: executable("crates/relay/alondra_relay_test"),
            arguments: ["--ignored", "--exact", "http_tests::process_fixture::full_stack_fixture_relay", "--nocapture"],
            home: home, prefix: "ALONDRA_FIXTURE ",
            additionalEnvironment: port.map { ["ALONDRA_FIXTURE_RELAY_PORT": String($0)] } ?? [:])
    }
    private func relayConfiguration(relay: RelayServerEvent, daemon: ServerEvent) throws -> ProcessClientConfiguration {
        .init(baseURL: "https://fixture-relay.invalid", dataEpoch: try #require(daemon.dataEpoch),
            actionID: try #require(daemon.actionID), deviceID: try #require(relay.deviceID),
            signingSeed: try #require(relay.signingSeed), optionID: daemon.optionID,
            relay: .init(baseURL: try #require(relay.baseURL), bearer: try #require(relay.bearer),
                macID: try #require(relay.macID), agreementSeed: try #require(relay.agreementSeed),
                macSigningSeed: try #require(relay.macSigningSeed), macAgreementSeed: try #require(relay.macAgreementSeed)))
    }

    @Test func fourColdReadsShareOneRealBootstrapBeforeAnyAnswerIsReleased() async throws {
        let relay = try launchRelay(home: TestFiles.directory())
        defer { relay.cleanup() }
        let relayReady = try await relay.next(RelayServerEvent.self)
        let daemon = try FixtureProcess(executable: executable("crates/alondrad/alondrad_test"),
            arguments: ["--ignored", "--exact", "operation_journal::process_fixture::full_stack_fixture_server", "--nocapture"],
            home: TestFiles.directory(), prefix: "ALONDRA_FIXTURE ",
            additionalEnvironment: ["ALONDRA_FIXTURE_RELAY_URL": try #require(relayReady.baseURL)])
        defer { daemon.cleanup() }
        let ready = try await daemon.next(ServerEvent.self)
        let clientHome = try TestFiles.directory(), config = clientHome.appending(path: "fixture.json")
        try JSONEncoder().encode(relayConfiguration(relay: relayReady, daemon: ready)).write(to: config)
        let client = try launchClient(home: clientHome, config: config)
        defer { client.cleanup() }
        #expect(try await client.next(ProcessClientEvent.self).event == "ready")
        try relay.send(ServerCommand(command: "stats"))
        let before = try await relay.next(RelayServerEvent.self)
        try client.send(ProcessClientCommand(command: "start_cold_reads"))
        #expect(try await client.next(ProcessClientEvent.self).event == "cold_bootstrap_held")
        try relay.send(ServerCommand(command: "stats"))
        let held = try await relay.next(RelayServerEvent.self)
        #expect(try #require(held.bootstrapGets) == #require(before.bootstrapGets) + 1)
        #expect(held.readPosts == before.readPosts, "all four readers are waiting for the same bootstrap")
        try client.send(ProcessClientCommand(command: "finish_cold_reads"))
        let reads = try await client.next(ProcessClientEvent.self)
        #expect(reads.event == "cold_reads_finished" && reads.readAttempts == 4 && reads.readErrors == 0)
        #expect(reads.detailKeys == ["card:fixture-detail-b", "card:fixture-detail-b", "card:fixture-detail-c", "card:fixture-detail-c"])
        try relay.send(ServerCommand(command: "stats"))
        let after = try await relay.next(RelayServerEvent.self)
        #expect(after.bootstrapGets == held.bootstrapGets)
        #expect(try #require(after.readPosts) == #require(before.readPosts) + 4)
        let completionMicroseconds = try #require(reads.coldCompletionMicroseconds)
        let bootstrapGets = try #require(after.bootstrapGets) - #require(before.bootstrapGets)
        print("cold relay reads attempts=\(reads.readAttempts ?? -1) errors=\(reads.readErrors ?? -1) client_bootstrap_gets=\(bootstrapGets) observed_completion_microseconds=\(completionMicroseconds)")
        try client.send(ProcessClientCommand(command: "quit"))
        #expect(try await client.next(ProcessClientEvent.self).event == "stopped")
        try daemon.send(ServerCommand(command: "stop"))
        #expect(try await daemon.next(ServerEvent.self).event == "stopped")
        try relay.send(ServerCommand(command: "stop"))
        #expect(try await relay.next(RelayServerEvent.self).event == "stopped")
    }

    @Test func aRealRelayRestartKeepsDetailsUsableAndRecoversTheAppliedActionWithoutAnotherPost() async throws {
        let relayHome = try TestFiles.directory()
        let relay = try launchRelay(home: relayHome)
        defer { relay.cleanup() }
        let relayReady = try await relay.next(RelayServerEvent.self)
        let relayURL = try #require(relayReady.baseURL)
        let port = try #require(URL(string: relayURL)?.port)
        let daemon = try FixtureProcess(executable: executable("crates/alondrad/alondrad_test"),
            arguments: ["--ignored", "--exact", "operation_journal::process_fixture::full_stack_fixture_server", "--nocapture"],
            home: TestFiles.directory(), prefix: "ALONDRA_FIXTURE ",
            additionalEnvironment: ["ALONDRA_FIXTURE_RELAY_URL": relayURL])
        defer { daemon.cleanup() }
        let ready = try await daemon.next(ServerEvent.self)
        let daemonPID = daemon.pid
        let clientHome = try TestFiles.directory(), config = clientHome.appending(path: "fixture.json")
        try JSONEncoder().encode(relayConfiguration(relay: relayReady, daemon: ready)).write(to: config)
        let client = try launchClient(home: clientHome, config: config)
        defer { client.cleanup() }
        #expect(try await client.next(ProcessClientEvent.self).event == "ready")
        try client.send(ProcessClientCommand(command: "prepare", nonce: "70000000-0000-4000-8000-000000000001"))
        let original = try #require(try await client.next(ProcessClientEvent.self).request)
        try relay.send(ServerCommand(command: "hold_next_action"))
        #expect(try await relay.next(RelayServerEvent.self).event == "armed")
        try client.send(ProcessClientCommand(command: "start_action"))
        #expect(try await client.next(ProcessClientEvent.self).event == "action_started")
        #expect(try await relay.next(RelayServerEvent.self).commandID == original.commandId)
        let applied = try await stats(daemon)
        #expect(applied.effects == 1 && applied.journalRecords == 1)
        try client.send(ProcessClientCommand(command: "measured_detail_burst"))
        let beforeReads = try await client.next(ProcessClientEvent.self)
        #expect(beforeReads.readAttempts == 8 && beforeReads.readErrors == 0)
        #expect(beforeReads.detailKeys == Array(repeating: "card:fixture-detail-b", count: 4) + Array(repeating: "card:fixture-detail-c", count: 4))
        try client.send(ProcessClientCommand(command: "transport_metrics"))
        let beforeTransport = try await client.next(ProcessClientEvent.self)
        #expect(beforeTransport.mutativeEnvelopePosts == 1 && beforeTransport.operationResultGets == 0)
        try relay.send(ServerCommand(command: "expire_action", requestID: original.commandId))
        let expired = try await relay.next(RelayServerEvent.self)
        #expect(expired.event == "expired" && expired.commandID == original.commandId)
        try relay.send(ServerCommand(command: "stats"))
        #expect(try await relay.next(RelayServerEvent.self).actionPosts == 1)
        #expect(relay.killAndWait() == SIGKILL)
        try client.send(ProcessClientCommand(command: "finish_action"))
        let uncertain = try await client.next(ProcessClientEvent.self)
        #expect(uncertain.event == "error" && uncertain.error == "outcomeUnknown")
        let restarted = try launchRelay(home: relayHome, port: port)
        defer { restarted.cleanup() }
        let restartedReady = try await restarted.next(RelayServerEvent.self)
        #expect(restarted.pid != relay.pid && restartedReady.baseURL == relayURL)
        #expect(restartedReady.signingSeed == relayReady.signingSeed && restartedReady.macAgreementSeed == relayReady.macAgreementSeed)
        try client.send(ProcessClientCommand(command: "measured_detail_burst"))
        let afterReads = try await client.next(ProcessClientEvent.self)
        #expect(afterReads.readAttempts == 8 && afterReads.readErrors == 0)
        #expect(afterReads.detailKeys == beforeReads.detailKeys)
        try client.send(ProcessClientCommand(command: "send", request: original))
        let recovered = try await client.next(ProcessClientEvent.self)
        #expect(recovered.event == "answered" && recovered.response?.outcome == .applied)
        #expect(recovered.request?.commandId == original.commandId)
        try client.send(ProcessClientCommand(command: "drain"))
        #expect(try await client.next(ProcessClientEvent.self).event == "retirement_drained")
        try client.send(ProcessClientCommand(command: "transport_metrics"))
        let afterTransport = try await client.next(ProcessClientEvent.self)
        #expect(afterTransport.mutativeEnvelopePosts == beforeTransport.mutativeEnvelopePosts)
        #expect(afterTransport.operationResultGets == 1 && afterTransport.operationResultIDs == [original.commandId])
        let final = try await stats(daemon)
        #expect(daemon.pid == daemonPID && final.effects == 1 && final.journalRecords == 0)
        try restarted.send(ServerCommand(command: "stats"))
        #expect(try await restarted.next(RelayServerEvent.self).actionPosts == 0)
        print("relay restart reads_before=\(beforeReads.readAttempts ?? -1) errors_before=\(beforeReads.readErrors ?? -1) reads_after=\(afterReads.readAttempts ?? -1) errors_after=\(afterReads.readErrors ?? -1) effects=\(final.effects ?? -1) fresh_result_gets=\(afterTransport.operationResultGets ?? -1) action_posts=1 daemon_pid=\(daemonPID)")
        try client.send(ProcessClientCommand(command: "quit"))
        #expect(try await client.next(ProcessClientEvent.self).event == "stopped")
        try daemon.send(ServerCommand(command: "stop"))
        #expect(try await daemon.next(ServerEvent.self).event == "stopped")
        try restarted.send(ServerCommand(command: "stop"))
        #expect(try await restarted.next(RelayServerEvent.self).event == "stopped")
    }

    @Test func actualClientProcessCutsRecoverTheOriginalSignedHTTPActionAndRetireItsReplay() async throws {
        let serverHome = try TestFiles.directory()
        let server = try FixtureProcess(executable: executable("crates/alondrad/alondrad_test"),
            arguments: ["--ignored", "--exact", "operation_journal::process_fixture::full_stack_fixture_server", "--nocapture"],
            home: serverHome, prefix: "ALONDRA_FIXTURE ")
        defer { server.cleanup() }
        let ready = try await server.next(ServerEvent.self)
        #expect(ready.event == "ready")
        let configuration = ProcessClientConfiguration(baseURL: try #require(ready.baseURL), dataEpoch: try #require(ready.dataEpoch),
            actionID: try #require(ready.actionID), deviceID: try #require(ready.deviceID), signingSeed: try #require(ready.signingSeed), optionID: ready.optionID)
        let clientHome = try TestFiles.directory(), config = clientHome.appending(path: "fixture.json")
        try JSONEncoder().encode(configuration).write(to: config)

        let first = try launchClient(home: clientHome, config: config)
        defer { first.cleanup() }
        let firstReady = try await first.next(ProcessClientEvent.self)
        #expect(firstReady.pid == first.pid)
        #expect(firstReady.pendingIDs == [])
        try first.send(ProcessClientCommand(command: "prepare", nonce: "00000000-0000-4000-8000-000000000001"))
        let prepared = try await first.next(ProcessClientEvent.self)
        #expect(prepared.event == "prepared")
        let original = try #require(prepared.request)
        #expect(first.killAndWait() == SIGKILL)
        let beforeDispatch = try await stats(server)
        #expect(beforeDispatch.effects == 0)
        #expect(beforeDispatch.actionPosts == 0)

        let second = try launchClient(home: clientHome, config: config)
        defer { second.cleanup() }
        let secondReady = try await second.next(ProcessClientEvent.self)
        #expect(secondReady.pid == second.pid && second.pid != first.pid)
        #expect(secondReady.pendingIDs == [original.commandId])
        #expect(secondReady.pendingPhases == ["prepared"])
        try second.send(ProcessClientCommand(command: "prepare", nonce: "00000000-0000-4000-8000-000000000002"))
        let adopted = try await second.next(ProcessClientEvent.self)
        #expect(adopted.request == original, "preparation adopts the original ticket before presentation")
        try server.send(ServerCommand(command: "hold_next_action"))
        #expect(try await server.next(ServerEvent.self).event == "armed")
        try second.send(ProcessClientCommand(command: "send"))
        let applied = try await server.next(ServerEvent.self)
        #expect(applied.event == "applied")
        #expect(applied.commandID == original.commandId)
        #expect(applied.effects == 1)
        #expect(second.killAndWait() == SIGKILL)
        try server.send(ServerCommand(command: "release"))
        #expect(try await server.next(ServerEvent.self).event == "released")
        let afterCut = try await stats(server)
        #expect(afterCut.effects == 1 && afterCut.actionPosts == 1)
        #expect(afterCut.resultGets == 0)

        let third = try launchClient(home: clientHome, config: config)
        defer { third.cleanup() }
        let thirdReady = try await third.next(ProcessClientEvent.self)
        #expect(thirdReady.pendingIDs == [original.commandId])
        #expect(thirdReady.pendingPhases == ["dispatched"])
        try server.send(ServerCommand(command: "hold_next_ack"))
        #expect(try await server.next(ServerEvent.self).event == "armed")
        try third.send(ProcessClientCommand(command: "send", request: original))
        #expect(try await server.next(ServerEvent.self).event == "ack_entered")
        #expect(third.killAndWait() == SIGKILL)
        try server.send(ServerCommand(command: "release"))
        #expect(try await server.next(ServerEvent.self).event == "released")

        let fourth = try launchClient(home: clientHome, config: config)
        defer { fourth.cleanup() }
        let fourthReady = try await fourth.next(ProcessClientEvent.self)
        #expect(fourthReady.pendingIDs == [], "completion survived the process cut before the ACK reply")
        try fourth.send(ProcessClientCommand(command: "send", request: original))
        let cached = try await fourth.next(ProcessClientEvent.self)
        #expect(cached.event == "answered" && cached.response?.outcome == .applied)
        try fourth.send(ProcessClientCommand(command: "reconcile"))
        #expect(try await fourth.next(ProcessClientEvent.self).event == "reconciled")
        try fourth.send(ProcessClientCommand(command: "drain"))
        #expect(try await fourth.next(ProcessClientEvent.self).event == "retirement_drained")
        let recovered = try await stats(server)
        #expect(recovered.effects == 1 && recovered.actionPosts == 1)
        #expect(recovered.resultGets == 1)
        #expect(try #require(recovered.acknowledgments) >= 2)
        #expect(recovered.journalRecords == 0)

        try fourth.send(ProcessClientCommand(command: "fill", count: 70))
        #expect(try await fourth.next(ProcessClientEvent.self).event == "filled")
        try fourth.send(ProcessClientCommand(command: "drain"))
        #expect(try await fourth.next(ProcessClientEvent.self).event == "retirement_drained")
        let compacted = try await stats(server)
        #expect(compacted.effects == 1 && compacted.actionPosts == 71)
        try fourth.send(ProcessClientCommand(command: "send", request: original))
        let retired = try await fourth.next(ProcessClientEvent.self)
        #expect(retired.event == "error" && retired.error == "outcomeUnknown")
        let onlyRead = try await stats(server)
        #expect(onlyRead.actionPosts == compacted.actionPosts)
        let previousGets = try #require(compacted.resultGets)
        #expect(onlyRead.resultGets == previousGets + 1)
        try fourth.send(ProcessClientCommand(command: "replay_http", request: original))
        let replay = try await fourth.next(ProcessClientEvent.self)
        #expect(replay.event == "error" && replay.status == 410)
        let final = try await stats(server)
        #expect(final.effects == 1)
        #expect(final.actionPosts == 72)
        #expect(final.journalRecords == 0)
        try fourth.send(ProcessClientCommand(command: "quit"))
        #expect(try await fourth.next(ProcessClientEvent.self).event == "stopped")
        try server.send(ServerCommand(command: "stop"))
        #expect(try await server.next(ServerEvent.self).event == "stopped")
    }

    @Test func aColdClientRecoversThroughTheRealEncryptedRelayWithoutAnotherMutativeEnvelope() async throws {
        let relayHome = try TestFiles.directory()
        let relay = try FixtureProcess(executable: executable("crates/relay/alondra_relay_test"),
            arguments: ["--ignored", "--exact", "http_tests::process_fixture::full_stack_fixture_relay", "--nocapture"],
            home: relayHome, prefix: "ALONDRA_FIXTURE ")
        defer { relay.cleanup() }
        let relayReady = try await relay.next(RelayServerEvent.self)
        #expect(relayReady.event == "ready")
        let relayURL = try #require(relayReady.baseURL), bearer = try #require(relayReady.bearer)
        let daemonHome = try TestFiles.directory()
        let token = "50000000-0000-4000-8000-000000000001"
        try Data("alondra-process-fixture:\(token)\n".utf8).write(to: daemonHome.appending(path: ".alondra-process-fixture"))
        let daemonEnvironment = ["ALONDRA_FIXTURE_RELAY_URL": relayURL,
            "ALONDRA_FIXTURE_HOME": daemonHome.path, "ALONDRA_FIXTURE_TOKEN": token]
        let daemon = try FixtureProcess(executable: executable("crates/alondrad/alondrad_test"),
            arguments: ["--ignored", "--exact", "operation_journal::process_fixture::full_stack_fixture_server", "--nocapture"],
            home: daemonHome, prefix: "ALONDRA_FIXTURE ",
            additionalEnvironment: daemonEnvironment)
        defer { daemon.cleanup() }
        let daemonReady = try await daemon.next(ServerEvent.self)
        #expect(daemonReady.event == "ready")
        let configuration = ProcessClientConfiguration(baseURL: "https://fixture-relay.invalid", dataEpoch: try #require(daemonReady.dataEpoch),
            actionID: try #require(daemonReady.actionID), deviceID: try #require(relayReady.deviceID), signingSeed: try #require(relayReady.signingSeed),
            optionID: daemonReady.optionID,
            relay: .init(baseURL: relayURL, bearer: bearer, macID: try #require(relayReady.macID),
                agreementSeed: try #require(relayReady.agreementSeed), macSigningSeed: try #require(relayReady.macSigningSeed),
                macAgreementSeed: try #require(relayReady.macAgreementSeed)))
        let clientHome = try TestFiles.directory(), config = clientHome.appending(path: "fixture.json")
        try JSONEncoder().encode(configuration).write(to: config)
        let first = try launchClient(home: clientHome, config: config)
        defer { first.cleanup() }
        #expect(try await first.next(ProcessClientEvent.self).event == "ready")
        try first.send(ProcessClientCommand(command: "prepare", nonce: "40000000-0000-4000-8000-000000000001"))
        let prepared = try await first.next(ProcessClientEvent.self)
        #expect(prepared.event == "prepared")
        let original = try #require(prepared.request)
        try relay.send(ServerCommand(command: "hold_next_action"))
        #expect(try await relay.next(RelayServerEvent.self).event == "armed")
        try first.send(ProcessClientCommand(command: "send"))
        let applied = try await relay.next(RelayServerEvent.self)
        #expect(applied.event == "applied" && applied.commandID == original.commandId)
        let durableBeforeCut = try await stats(daemon)
        #expect(durableBeforeCut.effects == 1 && durableBeforeCut.journalRecords == 1)
        #expect(first.killAndWait() == SIGKILL)
        #expect(daemon.killAndWait() == SIGKILL)
        try relay.send(ServerCommand(command: "release"))
        #expect(try await relay.next(RelayServerEvent.self).event == "released")
        let restarted = try FixtureProcess(executable: executable("crates/alondrad/alondrad_test"),
            arguments: ["--ignored", "--exact", "operation_journal::process_fixture::full_stack_fixture_server", "--nocapture"],
            home: daemonHome, prefix: "ALONDRA_FIXTURE ", additionalEnvironment: daemonEnvironment)
        defer { restarted.cleanup() }
        let restartedReady = try await restarted.next(ServerEvent.self)
        #expect(restartedReady.event == "ready")
        #expect(restarted.pid != daemon.pid)
        #expect(restartedReady.fixtureHome == daemonReady.fixtureHome)
        #expect(restartedReady.dataEpoch == daemonReady.dataEpoch)
        let afterEffect = try await stats(restarted)
        #expect(afterEffect.effects == 1)
        #expect(afterEffect.journalRecords == 1)

        let cold = try launchClient(home: clientHome, config: config)
        defer { cold.cleanup() }
        let restored = try await cold.next(ProcessClientEvent.self)
        #expect(restored.pendingIDs == [original.commandId] && restored.pendingPhases == ["dispatched"])
        try cold.send(ProcessClientCommand(command: "send", request: original))
        let recovered = try await cold.next(ProcessClientEvent.self)
        #expect(recovered.event == "answered" && recovered.response?.outcome == .applied)
        #expect(recovered.request?.commandId == original.commandId)
        try cold.send(ProcessClientCommand(command: "drain"))
        #expect(try await cold.next(ProcessClientEvent.self).event == "retirement_drained")
        try cold.send(ProcessClientCommand(command: "transport_metrics"))
        let transport = try await cold.next(ProcessClientEvent.self)
        #expect(transport.mutativeEnvelopePosts == 0, "cold recovery must not submit another op_ envelope")
        #expect(try #require(transport.otherEnvelopePosts) >= 2, "recovery and ACK are fresh authenticated envelopes")
        let final = try await stats(restarted)
        #expect(final.effects == 1 && final.journalRecords == 0)
        try relay.send(ServerCommand(command: "stats"))
        let relayStats = try await relay.next(RelayServerEvent.self)
        #expect(relayStats.event == "stats")
        #expect(relayStats.actionPosts == 1)
        #expect(try #require(relayStats.readPosts) >= 3, "descriptor, recovery and ACK use other envelope IDs")
        try cold.send(ProcessClientCommand(command: "quit"))
        #expect(try await cold.next(ProcessClientEvent.self).event == "stopped")
        try restarted.send(ServerCommand(command: "stop"))
        #expect(try await restarted.next(ServerEvent.self).event == "stopped")
        try relay.send(ServerCommand(command: "stop"))
        #expect(try await relay.next(RelayServerEvent.self).event == "stopped")
    }

    @Test(arguments: [false, true])
    func answeringACardAndImmediatelyOpeningOtherDetailsKeepsTheConnectionUsable(withRelay: Bool) async throws {
        let relay: FixtureProcess?
        let relayReady: RelayServerEvent?
        if withRelay {
            relay = try FixtureProcess(executable: executable("crates/relay/alondra_relay_test"),
                arguments: ["--ignored", "--exact", "http_tests::process_fixture::full_stack_fixture_relay", "--nocapture"],
                home: try TestFiles.directory(), prefix: "ALONDRA_FIXTURE ")
            relayReady = try await relay!.next(RelayServerEvent.self)
        } else { relay = nil; relayReady = nil }
        defer { relay?.cleanup() }
        let daemonHome = try TestFiles.directory()
        var environment: [String: String] = [:]
        if let relayReady { environment["ALONDRA_FIXTURE_RELAY_URL"] = try #require(relayReady.baseURL) }
        let daemon = try FixtureProcess(executable: executable("crates/alondrad/alondrad_test"),
            arguments: ["--ignored", "--exact", "operation_journal::process_fixture::full_stack_fixture_server", "--nocapture"],
            home: daemonHome, prefix: "ALONDRA_FIXTURE ", additionalEnvironment: environment)
        defer { daemon.cleanup() }
        let ready = try await daemon.next(ServerEvent.self)
        var config = ProcessClientConfiguration(baseURL: ready.baseURL ?? "https://fixture-relay.invalid",
            dataEpoch: try #require(ready.dataEpoch), actionID: try #require(ready.actionID),
            deviceID: try #require(ready.deviceID ?? relayReady?.deviceID), signingSeed: try #require(ready.signingSeed ?? relayReady?.signingSeed), optionID: ready.optionID)
        if let relayReady {
            config = .init(baseURL: "https://fixture-relay.invalid", dataEpoch: config.dataEpoch,
                actionID: config.actionID, deviceID: try #require(relayReady.deviceID), signingSeed: try #require(relayReady.signingSeed),
                optionID: config.optionID, relay: .init(baseURL: try #require(relayReady.baseURL), bearer: try #require(relayReady.bearer),
                    macID: try #require(relayReady.macID), agreementSeed: try #require(relayReady.agreementSeed),
                    macSigningSeed: try #require(relayReady.macSigningSeed), macAgreementSeed: try #require(relayReady.macAgreementSeed),
                    expiredBearer: try #require(relayReady.expiredBearer)))
        }
        let clientHome = try TestFiles.directory(), configFile = clientHome.appending(path: "fixture.json")
        try JSONEncoder().encode(config).write(to: configFile)
        let client = try launchClient(home: clientHome, config: configFile)
        defer { client.cleanup() }
        #expect(try await client.next(ProcessClientEvent.self).event == "ready")
        try client.send(ProcessClientCommand(command: "prepare", nonce: "60000000-0000-4000-8000-000000000001"))
        let prepared = try await client.next(ProcessClientEvent.self)
        let original = try #require(prepared.request)
        if withRelay {
            try client.send(ProcessClientCommand(command: "hold_stale_authorization"))
            #expect(try await client.next(ProcessClientEvent.self).event == "stale_authorization_held")
        }
        let responseGate = relay ?? daemon
        try responseGate.send(ServerCommand(command: "hold_next_action"))
        if let relay { #expect(try await relay.next(RelayServerEvent.self).event == "armed") }
        else { #expect(try await daemon.next(ServerEvent.self).event == "armed") }
        try client.send(ProcessClientCommand(command: "start_action"))
        #expect(try await client.next(ProcessClientEvent.self).event == "action_started")
        if let relay { #expect(try await relay.next(RelayServerEvent.self).commandID == original.commandId) }
        else { #expect(try await daemon.next(ServerEvent.self).commandID == original.commandId) }
        let before = try await stats(daemon)
        #expect(before.effects == 1)
        var verifiedDetails = 0
        var failedBursts = 0
        for round in 0..<2 {
            if withRelay, round == 1 {
                try client.send(ProcessClientCommand(command: "release_stale_authorization"))
                let finished = try await client.next(ProcessClientEvent.self)
                #expect(finished.event == "stale_authorization_finished" && finished.authorizationRefusals == 0)
            }
            try client.send(ProcessClientCommand(command: "detail_burst"))
            let details = try await client.next(ProcessClientEvent.self)
            if details.event == "details_loaded" { verifiedDetails += details.detailKeys?.count ?? 0 }
            else { failedBursts += 1 }
            #expect(details.event == "details_loaded")
            #expect(details.detailKeys == Array(repeating: "card:fixture-detail-b", count: 4)
                + Array(repeating: "card:fixture-detail-c", count: 4))
        }
        try responseGate.send(ServerCommand(command: "release"))
        if let relay { #expect(try await relay.next(RelayServerEvent.self).event == "released") }
        else { #expect(try await daemon.next(ServerEvent.self).event == "released") }
        try client.send(ProcessClientCommand(command: "finish_action"))
        let answer = try await client.next(ProcessClientEvent.self)
        #expect(answer.event == "answered" && answer.response?.outcome == .applied)
        let final = try await stats(daemon)
        #expect(final.effects == 1)
        if let relay {
            try relay.send(ServerCommand(command: "stats"))
            #expect(try await relay.next(RelayServerEvent.self).actionPosts == 1)
        } else { #expect(final.actionPosts == 1) }
        print("action-detail E2E relay=\(withRelay) verified_details=\(verifiedDetails) failed_bursts=\(failedBursts) action_effects=\(final.effects ?? -1)")
        try client.send(ProcessClientCommand(command: "quit"))
        #expect(try await client.next(ProcessClientEvent.self).event == "stopped")
        try daemon.send(ServerCommand(command: "stop"))
        #expect(try await daemon.next(ServerEvent.self).event == "stopped")
        if let relay {
            try relay.send(ServerCommand(command: "stop"))
            #expect(try await relay.next(RelayServerEvent.self).event == "stopped")
        }
    }

    @Test(arguments: 0..<3)
    func aSaturatedPhoneCannotKeepAnotherPhoneFromAnsweringAndOpeningConsecutiveDetails(cohort: Int) async throws {
        func elapsedMilliseconds(since start: ContinuousClock.Instant) -> Double {
            let duration = start.duration(to: ContinuousClock().now).components
            return Double(duration.seconds) * 1000 + Double(duration.attoseconds) / 1_000_000_000_000_000
        }
        let relay = try launchRelay(home: TestFiles.directory())
        defer { relay.cleanup() }
        let relayReady = try await relay.next(RelayServerEvent.self)
        let daemon = try FixtureProcess(executable: executable("crates/alondrad/alondrad_test"),
            arguments: ["--ignored", "--exact", "operation_journal::process_fixture::full_stack_fixture_server", "--nocapture"],
            home: try TestFiles.directory(), prefix: "ALONDRA_FIXTURE ",
            additionalEnvironment: ["ALONDRA_FIXTURE_RELAY_URL": try #require(relayReady.baseURL)])
        defer { daemon.cleanup() }
        let daemonReady = try await daemon.next(ServerEvent.self)
        let configuration = try relayConfiguration(relay: relayReady, daemon: daemonReady)
        let interactiveHome = try TestFiles.directory(), interactiveConfig = interactiveHome.appending(path: "fixture.json")
        try JSONEncoder().encode(configuration).write(to: interactiveConfig)
        let interactive = try launchClient(home: interactiveHome, config: interactiveConfig)
        defer { interactive.cleanup() }
        #expect(try await interactive.next(ProcessClientEvent.self).event == "ready")
        try interactive.send(ProcessClientCommand(command: "prepare", nonce: "70000000-0000-4000-8000-000000000002"))
        let preparedEvent = try await interactive.next(ProcessClientEvent.self)
        let answer = try #require(preparedEvent.request)
        #expect(preparedEvent.event == "prepared")

        let relayConfiguration = try #require(configuration.relay)
        let noisyConfiguration = ProcessClientConfiguration(baseURL: configuration.baseURL, dataEpoch: configuration.dataEpoch,
            actionID: configuration.actionID, deviceID: "noise-phone", signingSeed: Data(repeating: 35, count: 32).base64EncodedString(),
            optionID: configuration.optionID, relay: .init(baseURL: relayConfiguration.baseURL, bearer: relayConfiguration.bearer,
                macID: relayConfiguration.macID, agreementSeed: Data(repeating: 36, count: 32).base64EncodedString(),
                macSigningSeed: relayConfiguration.macSigningSeed, macAgreementSeed: relayConfiguration.macAgreementSeed))
        let noisyHome = try TestFiles.directory(), noisyConfig = noisyHome.appending(path: "fixture.json")
        try JSONEncoder().encode(noisyConfiguration).write(to: noisyConfig)
        let noisy = try launchClient(home: noisyHome, config: noisyConfig)
        defer { noisy.cleanup() }
        #expect(try await noisy.next(ProcessClientEvent.self).event == "ready")
        try noisy.send(ProcessClientCommand(command: "start_transfer"))
        #expect(try await noisy.next(ProcessClientEvent.self).event == "transfer_chunk_held")
        try relay.send(ServerCommand(command: "hold_work"))
        #expect(try await relay.next(RelayServerEvent.self).event == "work_armed")
        try noisy.send(ProcessClientCommand(command: "preload_noise"))
        let preloaded = try await noisy.next(ProcessClientEvent.self)
        let expectedNoiseIDs = (0..<32).map { String(format: "noise-%03d", $0) }
        #expect(preloaded.event == "noise_preloaded" && preloaded.envelopeIDs == expectedNoiseIDs)
        try relay.send(ServerCommand(command: "queue_stats"))
        let before = try await relay.next(RelayServerEvent.self)
        #expect(before.event == "queue_stats" && before.pendingNoise == 32)
        #expect(try #require(before.cachedPayloadBytes) > 0)
        #expect(try #require(before.reservedResponseBytes) >= 32 * 1024 * 1024)

        try noisy.send(ProcessClientCommand(command: "release_transfer"))
        #expect(try await noisy.next(ProcessClientEvent.self).event == "transfer_admission_rejected")
        try noisy.send(ProcessClientCommand(command: "start_blocked_read", key: "card:fixture-detail-c"))
        #expect(try await noisy.next(ProcessClientEvent.self).event == "read_admission_rejected")
        let actionStarted = ContinuousClock().now
        try interactive.send(ProcessClientCommand(command: "start_action"))
        #expect(try await interactive.next(ProcessClientEvent.self).event == "action_started")
        try interactive.send(ProcessClientCommand(command: "await_action_admitted"))
        #expect(try await interactive.next(ProcessClientEvent.self).envelopeIDs == [answer.commandId])
        var dispatchIDs: [String] = []
        for _ in 0..<2 {
            try relay.send(ServerCommand(command: "step_work"))
            let dispatched = try await relay.next(RelayServerEvent.self)
            #expect(dispatched.event == "work_dispatched")
            dispatchIDs.append(try #require(dispatched.commandID))
        }
        #expect(dispatchIDs.first == "noise-000")
        #expect(dispatchIDs.last == answer.commandId, "a second phone gets the next turn after one noisy request")
        var held = dispatchIDs.last == answer.commandId
        // Complete the finite workload even on the old FIFO so regression evidence is a failed assertion, not a hung process.
        if !held {
            try relay.send(ServerCommand(command: "release_work"))
            #expect(try await relay.next(RelayServerEvent.self).event == "work_released")
        }
        try interactive.send(ProcessClientCommand(command: "finish_action"))
        let applied = try await interactive.next(ProcessClientEvent.self)
        let actionElapsedMilliseconds = elapsedMilliseconds(since: actionStarted)
        var interactiveErrors = applied.event == "answered" ? 0 : 1
        var detailElapsedMilliseconds: [Double] = []
        #expect(applied.event == "answered" && applied.response?.outcome == .applied)
        #expect(try await stats(daemon).effects == 1)
        for key in ["card:fixture-detail-b", "card:fixture-detail-c"] {
            let detailStarted = ContinuousClock().now
            try interactive.send(ProcessClientCommand(command: "start_detail", key: key))
            let accepted = try await interactive.next(ProcessClientEvent.self)
            #expect(accepted.event == "detail_admitted")
            let id = try #require(accepted.envelopeIDs?.first)
            if held {
                var selected = false
                for _ in 0..<6 {
                    try relay.send(ServerCommand(command: "step_work"))
                    let dispatched = try await relay.next(RelayServerEvent.self)
                    let dispatchedID = try #require(dispatched.commandID)
                    dispatchIDs.append(dispatchedID)
                    if dispatchedID == id { selected = true; break }
                }
                #expect(selected, "consecutive details progress while the noisy phone still owns pending work")
                if !selected {
                    held = false
                    try relay.send(ServerCommand(command: "release_work"))
                    #expect(try await relay.next(RelayServerEvent.self).event == "work_released")
                }
            }
            try interactive.send(ProcessClientCommand(command: "finish_detail"))
            let detail = try await interactive.next(ProcessClientEvent.self)
            detailElapsedMilliseconds.append(elapsedMilliseconds(since: detailStarted))
            if detail.event != "detail_finished" { interactiveErrors += 1 }
            #expect(detail.event == "detail_finished" && detail.detailKeys == [key] && detail.readErrors == 0)
        }
        try relay.send(ServerCommand(command: "queue_stats"))
        let during = try await relay.next(RelayServerEvent.self)
        #expect(try #require(during.pendingNoise) >= 24, "interactive work finishes before the noisy backlog drains")
        if held {
            try relay.send(ServerCommand(command: "release_work"))
            #expect(try await relay.next(RelayServerEvent.self).event == "work_released")
        }
        try noisy.send(ProcessClientCommand(command: "finish_detail"))
        let resumedRead = try await noisy.next(ProcessClientEvent.self)
        #expect(resumedRead.event == "detail_finished" && resumedRead.detailKeys == ["card:fixture-detail-c"] && resumedRead.readErrors == 0)
        try noisy.send(ProcessClientCommand(command: "finish_transfer"))
        let transfer = try await noisy.next(ProcessClientEvent.self)
        #expect(transfer.event == "transfer_finished" && transfer.transferBytes == 192 * 1024 + 17)
        #expect(transfer.transferDigest?.count == 64)
        try noisy.send(ProcessClientCommand(command: "verify_noise"))
        let completed = try await noisy.next(ProcessClientEvent.self)
        #expect(completed.event == "noise_verified" && completed.envelopeIDs == expectedNoiseIDs && completed.readErrors == 0)
        try noisy.send(ProcessClientCommand(command: "saturation_metrics"))
        let metrics = try await noisy.next(ProcessClientEvent.self)
        #expect(metrics.event == "saturation_metrics" && metrics.immutableResubmissions == true)
        #expect(try #require(metrics.admissionRejections) >= 2)
        for client in [interactive, noisy] {
            try client.send(ProcessClientCommand(command: "drain_acknowledgments"))
            #expect(try await client.next(ProcessClientEvent.self).event == "acknowledgments_drained")
        }
        try relay.send(ServerCommand(command: "queue_stats"))
        let after = try await relay.next(RelayServerEvent.self)
        #expect(after.pendingNoise == 0 && after.pending == 0)
        #expect(after.reservedResponseBytes == 0)
        #expect(after.admittedByteCharge == after.cachedPayloadBytes)
        let final = try await stats(daemon)
        #expect(final.effects == 1)
        let history = after.dispatchHistory ?? []
        let actionRank: Int
        if let firstNoise = history.firstIndex(of: "noise-000"), let answerIndex = history.firstIndex(of: answer.commandId) {
            actionRank = answerIndex - firstNoise + 1
        } else { actionRank = 0 }
        #expect(actionRank == 2)
        func queueMetrics(_ event: RelayServerEvent) -> [String: Int] {
            ["pending": event.pending ?? -1, "pending_noise": event.pendingNoise ?? -1,
             "cached_payload_bytes": event.cachedPayloadBytes ?? -1,
             "reserved_response_bytes": event.reservedResponseBytes ?? -1,
             "admitted_byte_charge": event.admittedByteCharge ?? -1]
        }
        let measured: [String: Any] = [
            "cohort": cohort, "action_relative_rank": actionRank, "dispatch_history": history,
            "controlled_dispatch_ids": dispatchIDs, "action_elapsed_ms": actionElapsedMilliseconds,
            "detail_elapsed_ms": detailElapsedMilliseconds, "interactive_errors": interactiveErrors,
            "noisy_read_errors": resumedRead.event == "detail_finished" ? resumedRead.readErrors ?? 0 : 1,
            "transfer_errors": transfer.event == "transfer_finished" ? 0 : 1,
            "transfer_bytes": transfer.transferBytes ?? 0,
            "noise_verification_errors": completed.event == "noise_verified" ? completed.readErrors ?? 0 : 1,
            "effects": final.effects ?? -1, "before": queueMetrics(before), "during": queueMetrics(during),
            "after": queueMetrics(after), "client_metrics": try JSONSerialization.jsonObject(with: JSONEncoder().encode(metrics))
        ]
        print("saturation E2E " + String(decoding: try JSONSerialization.data(withJSONObject: measured, options: [.sortedKeys]), as: UTF8.self))
        for client in [interactive, noisy] {
            try client.send(ProcessClientCommand(command: "quit"))
            #expect(try await client.next(ProcessClientEvent.self).event == "stopped")
        }
        try daemon.send(ServerCommand(command: "stop"))
        #expect(try await daemon.next(ServerEvent.self).event == "stopped")
        try relay.send(ServerCommand(command: "stop"))
        #expect(try await relay.next(RelayServerEvent.self).event == "stopped")
    }

    @Test func actualPreviousStoreGrowthIsComparedWithSQLiteAfterTheResponseCacheFills() async throws {
        let home = try TestFiles.directory(), config = home.appending(path: "fixture.json")
        let configuration = ProcessClientConfiguration(baseURL: "http://127.0.0.1:1", dataEpoch: "history-epoch",
            actionID: "unused", deviceID: "fixture", signingSeed: Data(repeating: 19, count: 32).base64EncodedString())
        try JSONEncoder().encode(configuration).write(to: config)
        let client = try launchClient(home: home, config: config)
        defer { client.cleanup() }
        #expect(try await client.next(ProcessClientEvent.self).event == "ready")
        try client.send(ProcessClientCommand(command: "history"))
        let result = try await client.next(ProcessClientEvent.self)
        #expect(result.event == "history")
        let samples = try #require(result.history)
        #expect(samples.map(\.operations) == [64, 128, 256, 1024])
        let middle = try #require(samples.first(where: { $0.operations == 128 }))
        let end = try #require(samples.last)
        #expect(end.previousFileBytes > middle.previousFileBytes)
        #expect(end.previousCiphertextPayloadWrittenBytes > 3 * middle.previousCiphertextPayloadWrittenBytes)
        #expect(end.sqliteFileBytes <= middle.sqliteFileBytes + 4 * 4096)
        #expect(end.sqliteEncryptedBytes <= middle.sqliteEncryptedBytes + 1024)
        #expect(end.sqlitePending == 0 && end.sqliteRecent == 64 && end.sqliteIntervals == 1)
        print("action-intent-history " + String(decoding: try JSONEncoder().encode(samples), as: UTF8.self))
        try client.send(ProcessClientCommand(command: "quit"))
        #expect(try await client.next(ProcessClientEvent.self).event == "stopped")
    }

    /// A real relay and daemon, and the configuration a client process needs to reach them.
    private struct RelayStack {
        let relay: FixtureProcess
        let daemon: FixtureProcess
        let config: URL
        let clientHome: URL

        func stop() async throws {
            try daemon.send(ServerCommand(command: "stop"))
            #expect(try await daemon.next(ServerEvent.self).event == "stopped")
            try relay.send(ServerCommand(command: "stop"))
            #expect(try await relay.next(RelayServerEvent.self).event == "stopped")
        }
    }

    private func relayStack() async throws -> RelayStack {
        let relay = try FixtureProcess(executable: executable("crates/relay/alondra_relay_test"),
            arguments: ["--ignored", "--exact", "http_tests::process_fixture::full_stack_fixture_relay", "--nocapture"],
            home: try TestFiles.directory(), prefix: "ALONDRA_FIXTURE ")
        let relayReady = try await relay.next(RelayServerEvent.self)
        #expect(relayReady.event == "ready")
        let relayURL = try #require(relayReady.baseURL)
        let daemonHome = try TestFiles.directory()
        let token = "60000000-0000-4000-8000-000000000001"
        try Data("alondra-process-fixture:\(token)\n".utf8).write(to: daemonHome.appending(path: ".alondra-process-fixture"))
        let daemon = try FixtureProcess(executable: executable("crates/alondrad/alondrad_test"),
            arguments: ["--ignored", "--exact", "operation_journal::process_fixture::full_stack_fixture_server", "--nocapture"],
            home: daemonHome, prefix: "ALONDRA_FIXTURE ",
            additionalEnvironment: ["ALONDRA_FIXTURE_RELAY_URL": relayURL, "ALONDRA_FIXTURE_HOME": daemonHome.path, "ALONDRA_FIXTURE_TOKEN": token])
        let daemonReady = try await daemon.next(ServerEvent.self)
        #expect(daemonReady.event == "ready")
        let configuration = ProcessClientConfiguration(baseURL: "https://fixture-relay.invalid", dataEpoch: try #require(daemonReady.dataEpoch),
            actionID: try #require(daemonReady.actionID), deviceID: try #require(relayReady.deviceID), signingSeed: try #require(relayReady.signingSeed),
            optionID: daemonReady.optionID,
            relay: .init(baseURL: relayURL, bearer: try #require(relayReady.bearer), macID: try #require(relayReady.macID),
                agreementSeed: try #require(relayReady.agreementSeed), macSigningSeed: try #require(relayReady.macSigningSeed),
                macAgreementSeed: try #require(relayReady.macAgreementSeed)))
        let clientHome = try TestFiles.directory(), config = clientHome.appending(path: "fixture.json")
        try JSONEncoder().encode(configuration).write(to: config)
        return RelayStack(relay: relay, daemon: daemon, config: config, clientHome: clientHome)
    }

    @Test func anAnswerOverAnUnreliableNetworkReachesTheMacOnceAndComesBackApplied() async throws {
        let stack = try await relayStack()
        defer { stack.relay.cleanup(); stack.daemon.cleanup() }
        let client = try launchClient(home: stack.clientHome, config: stack.config)
        defer { client.cleanup() }
        #expect(try await client.next(ProcessClientEvent.self).event == "ready")
        try client.send(ProcessClientCommand(command: "prepare", nonce: "60000000-0000-4000-8000-000000000002"))
        let prepared = try #require(try await client.next(ProcessClientEvent.self).request)

        try client.send(ProcessClientCommand(command: "network", network: .init(cutAnswers: 1, lostReplies: 1, lostStatus: 2)))
        #expect(try await client.next(ProcessClientEvent.self).event == "network_set")
        try client.send(ProcessClientCommand(command: "deliver"))
        let delivered = try await client.next(ProcessClientEvent.self)

        #expect(delivered.event == "answered", "the answer arrives on its own, with no error: \(delivered.error ?? "")")
        #expect(delivered.response?.outcome == .applied)
        #expect(delivered.pendingIDs == [] && delivered.undelivered == [], "nothing is left to send and no card is held back")
        try client.send(ProcessClientCommand(command: "transport_metrics"))
        let transport = try await client.next(ProcessClientEvent.self)
        #expect(try #require(transport.mutativeEnvelopePosts) >= 2, "one cut before the relay, one whose reply was lost")
        #expect(try await stats(stack.daemon).effects == 1)

        try client.send(ProcessClientCommand(command: "resend", request: prepared))
        let duplicate = try await client.next(ProcessClientEvent.self)
        #expect(duplicate.event == "replayed" && duplicate.response?.outcome == .applied,
                "the same sealed answer again is answered with what the Mac already did")
        try stack.relay.send(ServerCommand(command: "stats"))
        let relayStats = try await stack.relay.next(RelayServerEvent.self)
        #expect(try #require(relayStats.actionPosts) >= 2, "the relay received the same answer more than once")
        #expect(try await stats(stack.daemon).effects == 1, "and the Mac ran it once")

        try client.send(ProcessClientCommand(command: "deliver", request: prepared))
        let again = try await client.next(ProcessClientEvent.self)
        #expect(again.event == "answered" && again.response?.outcome == .applied, "delivering it again only reads what this device kept")
        #expect(try await stats(stack.daemon).effects == 1)

        try client.send(ProcessClientCommand(command: "quit"))
        #expect(try await client.next(ProcessClientEvent.self).event == "stopped")
        try await stack.stop()
    }

    @Test func anAnswerCutOffUntilTheAppIsKilledIsDeliveredAfterTheRelaunchAndItsCardStaysOutMeanwhile() async throws {
        let stack = try await relayStack()
        defer { stack.relay.cleanup(); stack.daemon.cleanup() }
        let first = try launchClient(home: stack.clientHome, config: stack.config)
        defer { first.cleanup() }
        #expect(try await first.next(ProcessClientEvent.self).event == "ready")
        try first.send(ProcessClientCommand(command: "prepare", nonce: "60000000-0000-4000-8000-000000000003"))
        let prepared = try #require(try await first.next(ProcessClientEvent.self).request)
        try first.send(ProcessClientCommand(command: "network", network: .init(cutAnswers: 1_000)))
        #expect(try await first.next(ProcessClientEvent.self).event == "network_set")
        try first.send(ProcessClientCommand(command: "start_delivery"))
        #expect(try await first.next(ProcessClientEvent.self).event == "delivery_started")
        try first.send(ProcessClientCommand(command: "await_answer_attempt"))
        #expect(try await first.next(ProcessClientEvent.self).event == "answer_attempted")
        #expect(first.killAndWait() == SIGKILL)
        #expect(try await stats(stack.daemon).effects == 0, "nothing reached the Mac")

        let relaunched = try launchClient(home: stack.clientHome, config: stack.config)
        defer { relaunched.cleanup() }
        let ready = try await relaunched.next(ProcessClientEvent.self)
        #expect(ready.pendingIDs == [prepared.commandId], "the answer survived the kill")
        #expect(ready.undelivered?.count == 1, "its card stays answered in the queue drawn after the relaunch")
        try relaunched.send(ProcessClientCommand(command: "resume"))
        let resumed = try await relaunched.next(ProcessClientEvent.self)
        #expect(resumed.event == "resumed")
        #expect(resumed.pendingIDs == [] && resumed.undelivered == [])
        #expect(try await stats(stack.daemon).effects == 1)

        try relaunched.send(ProcessClientCommand(command: "quit"))
        #expect(try await relaunched.next(ProcessClientEvent.self).event == "stopped")
        try await stack.stop()
    }
}
