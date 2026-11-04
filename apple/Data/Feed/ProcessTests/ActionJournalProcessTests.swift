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
    static let hangGuard: TimeInterval = 240
    let pid: Int32
    private let lifecycle = Mutex(Lifecycle())
    private let input: FileHandle
    private let output: FileHandle
    private let errorOutput: FileHandle
    private let errors: URL
    private let reader = DispatchQueue(label: "alondra.process-fixture-reader")
    private let watchdog = DispatchQueue(label: "alondra.process-fixture-watchdog")
    private let prefix: String

    init(executable: URL, arguments: [String], home: URL, prefix: String, additionalEnvironment: [String: String] = [:]) throws {
        let stdin = Pipe(), stdout = Pipe()
        input = stdin.fileHandleForWriting; output = stdout.fileHandleForReading
        self.prefix = prefix
        errors = home.appending(path: "process-errors.log")
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
        timer.schedule(deadline: .now() + Self.hangGuard)
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
                                let stderr = (try? String(contentsOf: self.errors, encoding: .utf8)) ?? ""
                                print("fixture pid=\(self.pid) closed its output; its stderr ends:\n\(stderr.suffix(4096))")
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
    var macID: String?
    var agreementSeed: String?
    var macSigningSeed: String?
    var macAgreementSeed: String?
    var irohEndpoint: String?
    var irohAddress: String?
    var irohRelay: String?
    var stage: String?
    var us: Double?
    var commandID: String?
    var effects: Int?
    var actionPosts: Int?
    var resultGets: Int?
    var acknowledgments: Int?
    var journalRecords: Int?
    enum CodingKeys: String, CodingKey {
        case event, effects, acknowledgments, stage, us
        case baseURL = "base_url", dataEpoch = "data_epoch", actionID = "action_id"
        case fixtureHome = "fixture_home", optionID = "option_id", deviceID = "device_id", signingSeed = "signing_seed_base64", commandID = "command_id"
        case macID = "mac_id", agreementSeed = "agreement_seed_base64"
        case macSigningSeed = "mac_signing_seed_base64", macAgreementSeed = "mac_agreement_seed_base64"
        case irohEndpoint = "iroh_endpoint", irohAddress = "iroh_address"
        case irohRelay = "iroh_relay"
        case actionPosts = "action_posts", resultGets = "result_gets", journalRecords = "journal_records"
    }
}

/// The daemon fixture serving the paired phone over its iroh endpoint on loopback, and the
/// configuration a client process needs to reach it.
private struct IrohStack {
    let daemon: FixtureProcess
    let ready: ServerEvent
    let config: URL
    let clientHome: URL

    func stop() async throws {
        try daemon.send(ServerCommand(command: "stop"))
        #expect(try await daemon.next(ServerEvent.self).event == "stopped")
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
    private func launchDaemon(home: URL, environment: [String: String] = [:]) throws -> FixtureProcess {
        try FixtureProcess(executable: executable("crates/alondrad/alondrad_test"),
            arguments: ["--ignored", "--exact", "operation_journal::process_fixture::full_stack_fixture_server", "--nocapture"],
            home: home, prefix: "ALONDRA_FIXTURE ", additionalEnvironment: environment)
    }
    private func stats(_ server: FixtureProcess) async throws -> ServerEvent {
        try server.send(ServerCommand(command: "stats"))
        let result = try await server.next(ServerEvent.self)
        #expect(result.event == "stats")
        return result
    }
    private func irohConfiguration(_ ready: ServerEvent) throws -> ProcessClientConfiguration {
        let macID = try #require(ready.macID)
        return .init(baseURL: "alondra-direct://" + macID, dataEpoch: try #require(ready.dataEpoch),
            actionID: try #require(ready.actionID), deviceID: try #require(ready.deviceID),
            signingSeed: try #require(ready.signingSeed), optionID: ready.optionID,
            iroh: .init(endpoint: try #require(ready.irohEndpoint), address: try #require(ready.irohAddress), macID: macID,
                agreementSeed: try #require(ready.agreementSeed), macSigningSeed: try #require(ready.macSigningSeed),
                macAgreementSeed: try #require(ready.macAgreementSeed), relay: ready.irohRelay))
    }
    private func irohStack() async throws -> IrohStack {
        let daemon = try launchDaemon(home: TestFiles.directory(), environment: ["ALONDRA_FIXTURE_IROH": "1"])
        let ready = try await daemon.next(ServerEvent.self)
        #expect(ready.event == "ready")
        let clientHome = try TestFiles.directory(), config = clientHome.appending(path: "fixture.json")
        try JSONEncoder().encode(irohConfiguration(ready)).write(to: config)
        return IrohStack(daemon: daemon, ready: ready, config: config, clientHome: clientHome)
    }

    /// Opt-in measurement against a disposable local iroh relay; elapsed time is evidence,
    /// never a test assertion. The daemon disables its IP transports for the relayed cases.
    @Test(.enabled(if: ProcessInfo.processInfo.environment["ALONDRA_MEASURE_RELAY"] != nil),
          .timeLimit(.minutes(5)),
          arguments: [false, true], ["cold", "live", "background", "stale"])
    func measureAnswerThroughEachConnection(relay: Bool, mode: String) async throws {
        let cold = mode == "cold", background = mode == "background"
        var environment = ["ALONDRA_FIXTURE_IROH": "1", "ALONDRA_FIXTURE_MEASURE": "1"]
        if relay { environment["ALONDRA_FIXTURE_IROH_RELAY"] = ProcessInfo.processInfo.environment["ALONDRA_MEASURE_RELAY"] }
        let daemon = try launchDaemon(home: TestFiles.directory(), environment: environment)
        defer { daemon.cleanup() }
        let ready = try await daemon.next(ServerEvent.self)
        var configuration = try irohConfiguration(ready)
        configuration.measure = true; configuration.startsLive = !cold
        let home = try TestFiles.directory(), config = home.appending(path: "fixture.json")
        try JSONEncoder().encode(configuration).write(to: config)
        let client = try launchClient(home: home, config: config)
        defer { client.cleanup() }
        #expect(try await client.next(ProcessClientEvent.self).event == "ready")
        if !cold {
            try client.send(ProcessClientCommand(command: "await_connections", count: 1))
            #expect(try await client.next(ProcessClientEvent.self).event == "connected")
        }
        if background {
            try daemon.send(ServerCommand(command: "hold_next_action"))
            #expect(try await daemon.next(ServerEvent.self).event == "armed")
        }
        var serverTimes: [String: Double] = [:]
        var commandID: String?
        func nextServer() async throws -> ServerEvent {
            while true {
                let event = try await daemon.next(ServerEvent.self)
                guard event.event == "timing" else { return event }
                if event.commandID == commandID, let stage = event.stage, let us = event.us { serverTimes[stage] = us }
            }
        }
        try client.send(ProcessClientCommand(command: "prepare", nonce: "80000000-0000-4000-8000-000000000001"))
        let request = try #require(try await client.next(ProcessClientEvent.self).request)
        commandID = request.commandId
        if mode == "stale" {
            try client.send(ProcessClientCommand(command: "network", network: .init(staleAnswers: 1)))
            #expect(try await client.next(ProcessClientEvent.self).event == "network_set")
        }
        if background {
            try client.send(ProcessClientCommand(command: "start_delivery"))
            #expect(try await client.next(ProcessClientEvent.self).event == "delivery_started")
            #expect(try await nextServer().event == "applied")
            try client.send(ProcessClientCommand(command: "go_idle"))
            #expect(try await client.next(ProcessClientEvent.self).event == "idle")
            try daemon.send(ServerCommand(command: "release"))
            #expect(try await nextServer().event == "released")
            try client.send(ProcessClientCommand(command: "finish_action"))
        } else {
            try client.send(ProcessClientCommand(command: "deliver"))
        }
        let answer = try await client.next(ProcessClientEvent.self)
        #expect(answer.event == "answered" && answer.response?.outcome == .applied)
        try client.send(ProcessClientCommand(command: "drain"))
        #expect(try await client.next(ProcessClientEvent.self).event == "retirement_drained")
        try client.send(ProcessClientCommand(command: "timings"))
        let measured = try await client.next(ProcessClientEvent.self)
        let samples = try #require(measured.timings)
        try daemon.send(ServerCommand(command: "stats"))
        let final = try await nextServer()
        #expect(final.event == "stats" && final.effects == 1 && final.actionPosts == 1)
        func time(_ stage: String, operation: String? = nil) throws -> Double {
            try #require(samples.first { $0.stage == stage && (operation == nil || $0.operation == operation) }?.us)
        }
        let tap = try time("tap"), firstSend = try time("action_send", operation: request.commandId)
        let sends = samples.filter { $0.stage == "action_send" && $0.operation == request.commandId }
        let sent = try #require(sends.last?.us)
        let received = try #require(serverTimes["received"]), committed = try #require(serverTimes["committed"])
        let dialBegin = try time("dial_begin"), dialEnd = try time("dial_end")
        let helloBegin = try time("hello_begin"), helloEnd = try time("hello_end")
        let report: [String: Any] = [
            "relay": relay, "mode": mode, "connections": measured.connections ?? -1,
            "tap_to_send_ms": (firstSend - tap) / 1000, "retry_and_recovery_ms": (sent - firstSend) / 1000,
            "action_attempts": sends.count, "dial_ms": (dialEnd - dialBegin) / 1000,
            "hello_ms": (helloEnd - helloBegin) / 1000,
            "send_to_daemon_ms": (received - sent) / 1000, "daemon_ms": (committed - received) / 1000,
            "tap_to_commit_ms": (committed - tap) / 1000,
            "tap_to_confirmation_ms": (try time("answered") - tap) / 1000,
            "client_trace": try JSONSerialization.jsonObject(with: JSONEncoder().encode(samples)),
            "daemon_trace": serverTimes,
        ]
        print("ANSWER_LATENCY " + String(decoding: try JSONSerialization.data(withJSONObject: report, options: [.sortedKeys]), as: UTF8.self))
        try client.send(ProcessClientCommand(command: "quit"))
        #expect(try await client.next(ProcessClientEvent.self).event == "stopped")
        try daemon.send(ServerCommand(command: "stop"))
        #expect(try await daemon.next(ServerEvent.self).event == "stopped")
    }

    @Test func theMacsIrohEndpointComingBackKeepsDetailsUsableAndRecoversTheAppliedActionWithoutAnotherSend() async throws {
        let stack = try await irohStack()
        defer { stack.daemon.cleanup() }
        let daemon = stack.daemon, daemonPID = daemon.pid
        let client = try launchClient(home: stack.clientHome, config: stack.config)
        defer { client.cleanup() }
        #expect(try await client.next(ProcessClientEvent.self).event == "ready")
        try client.send(ProcessClientCommand(command: "prepare", nonce: "70000000-0000-4000-8000-000000000001"))
        let original = try #require(try await client.next(ProcessClientEvent.self).request)
        try daemon.send(ServerCommand(command: "hold_next_action"))
        #expect(try await daemon.next(ServerEvent.self).event == "armed")
        try client.send(ProcessClientCommand(command: "start_action"))
        #expect(try await client.next(ProcessClientEvent.self).event == "action_started")
        #expect(try await daemon.next(ServerEvent.self).commandID == original.commandId)
        let applied = try await stats(daemon)
        #expect(applied.effects == 1 && applied.journalRecords == 1)
        try client.send(ProcessClientCommand(command: "measured_detail_burst"))
        let beforeReads = try await client.next(ProcessClientEvent.self)
        #expect(beforeReads.readAttempts == 8 && beforeReads.readErrors == 0)
        #expect(beforeReads.detailKeys == Array(repeating: "card:fixture-detail-b", count: 4) + Array(repeating: "card:fixture-detail-c", count: 4))
        try client.send(ProcessClientCommand(command: "transport_metrics"))
        let beforeTransport = try await client.next(ProcessClientEvent.self)
        #expect(beforeTransport.mutativeEnvelopePosts == 1 && beforeTransport.operationResultGets == 0)
        try client.send(ProcessClientCommand(command: "connections"))
        let connections = try #require(try await client.next(ProcessClientEvent.self).connections)

        try daemon.send(ServerCommand(command: "close_endpoint"))
        #expect(try await daemon.next(ServerEvent.self).event == "endpoint_closed")
        try client.send(ProcessClientCommand(command: "finish_action"))
        let uncertain = try await client.next(ProcessClientEvent.self)
        #expect(uncertain.event == "error" && uncertain.error == "outcomeUnknown")
        try daemon.send(ServerCommand(command: "open_endpoint"))
        let reopened = try await daemon.next(ServerEvent.self)
        #expect(reopened.event == "endpoint_open" && reopened.irohAddress == stack.ready.irohAddress)
        try client.send(ProcessClientCommand(command: "await_connections", count: connections + 1))
        #expect(try await client.next(ProcessClientEvent.self).event == "connected")

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
        #expect(final.actionPosts == 1, "the Mac got the action once; recovery only read its stored answer")
        print("iroh endpoint back reads_before=\(beforeReads.readAttempts ?? -1) errors_before=\(beforeReads.readErrors ?? -1) reads_after=\(afterReads.readAttempts ?? -1) errors_after=\(afterReads.readErrors ?? -1) effects=\(final.effects ?? -1) result_gets=\(afterTransport.operationResultGets ?? -1) action_posts=\(final.actionPosts ?? -1) daemon_pid=\(daemonPID)")
        try client.send(ProcessClientCommand(command: "quit"))
        #expect(try await client.next(ProcessClientEvent.self).event == "stopped")
        try await stack.stop()
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

    @Test func aColdClientRecoversOverIrohAfterTheMacRestartsWithoutAnotherMutativeEnvelope() async throws {
        let daemonHome = try TestFiles.directory()
        let token = "50000000-0000-4000-8000-000000000001"
        try Data("alondra-process-fixture:\(token)\n".utf8).write(to: daemonHome.appending(path: ".alondra-process-fixture"))
        var daemonEnvironment = ["ALONDRA_FIXTURE_IROH": "1", "ALONDRA_FIXTURE_HOME": daemonHome.path, "ALONDRA_FIXTURE_TOKEN": token]
        let daemon = try launchDaemon(home: daemonHome, environment: daemonEnvironment)
        defer { daemon.cleanup() }
        let daemonReady = try await daemon.next(ServerEvent.self)
        #expect(daemonReady.event == "ready")
        let clientHome = try TestFiles.directory(), config = clientHome.appending(path: "fixture.json")
        try JSONEncoder().encode(irohConfiguration(daemonReady)).write(to: config)
        let first = try launchClient(home: clientHome, config: config)
        defer { first.cleanup() }
        #expect(try await first.next(ProcessClientEvent.self).event == "ready")
        try first.send(ProcessClientCommand(command: "prepare", nonce: "40000000-0000-4000-8000-000000000001"))
        let prepared = try await first.next(ProcessClientEvent.self)
        #expect(prepared.event == "prepared")
        let original = try #require(prepared.request)
        try daemon.send(ServerCommand(command: "hold_next_action"))
        #expect(try await daemon.next(ServerEvent.self).event == "armed")
        try first.send(ProcessClientCommand(command: "send"))
        let applied = try await daemon.next(ServerEvent.self)
        #expect(applied.event == "applied" && applied.commandID == original.commandId)
        let durableBeforeCut = try await stats(daemon)
        #expect(durableBeforeCut.effects == 1 && durableBeforeCut.journalRecords == 1)
        #expect(first.killAndWait() == SIGKILL)
        #expect(daemon.killAndWait() == SIGKILL)

        let address = try #require(daemonReady.irohAddress)
        daemonEnvironment["ALONDRA_FIXTURE_IROH_PORT"] = try #require(address.split(separator: ":").last.map(String.init))
        let restarted = try launchDaemon(home: daemonHome, environment: daemonEnvironment)
        defer { restarted.cleanup() }
        let restartedReady = try await restarted.next(ServerEvent.self)
        #expect(restartedReady.event == "ready")
        #expect(restarted.pid != daemon.pid)
        #expect(restartedReady.fixtureHome == daemonReady.fixtureHome)
        #expect(restartedReady.dataEpoch == daemonReady.dataEpoch)
        #expect(restartedReady.irohEndpoint == daemonReady.irohEndpoint && restartedReady.irohAddress == address)
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
        #expect(transport.mutativeEnvelopePosts == 0, "cold recovery must not send another op_ envelope")
        #expect(try #require(transport.otherEnvelopePosts) >= 2, "recovery and ACK are fresh sealed requests")
        let final = try await stats(restarted)
        #expect(final.effects == 1 && final.journalRecords == 0)
        #expect(final.actionPosts == 0 && final.resultGets == 1, "the restarted Mac only read back its stored answer")
        try cold.send(ProcessClientCommand(command: "quit"))
        #expect(try await cold.next(ProcessClientEvent.self).event == "stopped")
        try restarted.send(ServerCommand(command: "stop"))
        #expect(try await restarted.next(ServerEvent.self).event == "stopped")
    }

    @Test(arguments: [false, true])
    func answeringACardAndImmediatelyOpeningOtherDetailsKeepsTheConnectionUsable(overIroh: Bool) async throws {
        let daemon = try launchDaemon(home: TestFiles.directory(), environment: overIroh ? ["ALONDRA_FIXTURE_IROH": "1"] : [:])
        defer { daemon.cleanup() }
        let ready = try await daemon.next(ServerEvent.self)
        let config = overIroh ? try irohConfiguration(ready)
            : ProcessClientConfiguration(baseURL: try #require(ready.baseURL), dataEpoch: try #require(ready.dataEpoch),
                actionID: try #require(ready.actionID), deviceID: try #require(ready.deviceID),
                signingSeed: try #require(ready.signingSeed), optionID: ready.optionID)
        let clientHome = try TestFiles.directory(), configFile = clientHome.appending(path: "fixture.json")
        try JSONEncoder().encode(config).write(to: configFile)
        let client = try launchClient(home: clientHome, config: configFile)
        defer { client.cleanup() }
        #expect(try await client.next(ProcessClientEvent.self).event == "ready")
        try client.send(ProcessClientCommand(command: "prepare", nonce: "60000000-0000-4000-8000-000000000001"))
        let original = try #require(try await client.next(ProcessClientEvent.self).request)
        try daemon.send(ServerCommand(command: "hold_next_action"))
        #expect(try await daemon.next(ServerEvent.self).event == "armed")
        try client.send(ProcessClientCommand(command: "start_action"))
        #expect(try await client.next(ProcessClientEvent.self).event == "action_started")
        #expect(try await daemon.next(ServerEvent.self).commandID == original.commandId)
        #expect(try await stats(daemon).effects == 1)
        var verifiedDetails = 0
        for _ in 0..<2 {
            try client.send(ProcessClientCommand(command: "detail_burst"))
            let details = try await client.next(ProcessClientEvent.self)
            if details.event == "details_loaded" { verifiedDetails += details.detailKeys?.count ?? 0 }
            #expect(details.event == "details_loaded")
            #expect(details.detailKeys == Array(repeating: "card:fixture-detail-b", count: 4)
                + Array(repeating: "card:fixture-detail-c", count: 4))
        }
        try daemon.send(ServerCommand(command: "release"))
        #expect(try await daemon.next(ServerEvent.self).event == "released")
        try client.send(ProcessClientCommand(command: "finish_action"))
        let answer = try await client.next(ProcessClientEvent.self)
        #expect(answer.event == "answered" && answer.response?.outcome == .applied)
        let final = try await stats(daemon)
        #expect(final.effects == 1 && final.actionPosts == 1)
        print("action-detail E2E iroh=\(overIroh) verified_details=\(verifiedDetails) action_effects=\(final.effects ?? -1)")
        try client.send(ProcessClientCommand(command: "quit"))
        #expect(try await client.next(ProcessClientEvent.self).event == "stopped")
        try daemon.send(ServerCommand(command: "stop"))
        #expect(try await daemon.next(ServerEvent.self).event == "stopped")
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
        #expect(samples.map(\.operations) == [64, 128, 256, 512])
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

    @Test func anAnswerOverAnUnreliableNetworkReachesTheMacOnceAndComesBackApplied() async throws {
        let stack = try await irohStack()
        defer { stack.daemon.cleanup() }
        let client = try launchClient(home: stack.clientHome, config: stack.config)
        defer { client.cleanup() }
        #expect(try await client.next(ProcessClientEvent.self).event == "ready")
        try client.send(ProcessClientCommand(command: "prepare", nonce: "60000000-0000-4000-8000-000000000002"))
        let prepared = try #require(try await client.next(ProcessClientEvent.self).request)

        try client.send(ProcessClientCommand(command: "network", network: .init(cutAnswers: 1, lostReplies: 1, lostStatus: 2)))
        #expect(try await client.next(ProcessClientEvent.self).event == "network_set")
        try client.send(ProcessClientCommand(command: "hold_retirement"))
        #expect(try await client.next(ProcessClientEvent.self).event == "retirement_held",
                "the phone keeps the sealed answer until the duplicate below has gone out")
        try client.send(ProcessClientCommand(command: "deliver"))
        let delivered = try await client.next(ProcessClientEvent.self)

        #expect(delivered.event == "answered", "the answer arrives on its own, with no error: \(delivered.error ?? "")")
        #expect(delivered.response?.outcome == .applied)
        #expect(delivered.pendingIDs == [] && delivered.undelivered == [], "nothing is left to send and no card is held back")
        try client.send(ProcessClientCommand(command: "transport_metrics"))
        let transport = try await client.next(ProcessClientEvent.self)
        #expect(try #require(transport.mutativeEnvelopePosts) >= 2, "one cut before the Mac, one whose reply was lost")
        #expect(try await stats(stack.daemon).effects == 1)

        try client.send(ProcessClientCommand(command: "resend", request: prepared))
        let duplicate = try await client.next(ProcessClientEvent.self)
        #expect(duplicate.event == "replayed" && duplicate.response?.outcome == .applied,
                "the same sealed answer again is answered with what the Mac already did")
        let afterDuplicate = try await stats(stack.daemon)
        #expect(try #require(afterDuplicate.actionPosts) >= 2, "the Mac received the same answer more than once")
        #expect(afterDuplicate.effects == 1, "and ran it once")
        try client.send(ProcessClientCommand(command: "release_retirement"))
        #expect(try await client.next(ProcessClientEvent.self).event == "retirement_drained")

        try client.send(ProcessClientCommand(command: "deliver", request: prepared))
        let again = try await client.next(ProcessClientEvent.self)
        #expect(again.event == "answered" && again.response?.outcome == .applied, "delivering it again only reads what this device kept")
        #expect(try await stats(stack.daemon).effects == 1)

        try client.send(ProcessClientCommand(command: "quit"))
        #expect(try await client.next(ProcessClientEvent.self).event == "stopped")
        try await stack.stop()
    }

    @Test func anAnswerCutOffUntilTheAppIsKilledIsDeliveredAfterTheRelaunchAndItsCardStaysOutMeanwhile() async throws {
        let stack = try await irohStack()
        defer { stack.daemon.cleanup() }
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
