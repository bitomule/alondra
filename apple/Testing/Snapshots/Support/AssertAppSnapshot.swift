import Foundation
import SnapshotTesting
import XCTest

/// Bazel-aware wrapper around swift-snapshot-testing. Under Bazel `#filePath` never points at the
/// real source tree, so the snapshot directory comes from the environment:
///  - verify (plain `bazel test`): baselines are read from the runfiles at
///    `$TEST_SRCDIR/_main/<SNAPSHOT_ROOT>/<file>` and a missing baseline fails.
///  - record (`SNAPSHOT_TESTING_RECORD=all`): images are written to
///    `$TEST_UNDECLARED_OUTPUTS_DIR/<SNAPSHOT_ROOT>/<file>`, which `make snapshots-record` copies back.
///  - outside Bazel: the library's own behaviour.
@MainActor
func assertAppSnapshot<Value, Format>(
    of value: @autoclosure () throws -> Value,
    as snapshotting: Snapshotting<Value, Format>,
    named name: String? = nil,
    file filePath: StaticString = #filePath,
    testName: String = #function,
    line: UInt = #line
) {
    let environment = ProcessInfo.processInfo.environment
    let fileName = URL(fileURLWithPath: "\(filePath)").deletingPathExtension().lastPathComponent
    let snapshotRoot = environment["SNAPSHOT_ROOT"]

    let directory: String?
    let record: SnapshotTestingConfiguration.Record?

    if environment["SNAPSHOT_TESTING_RECORD"] == "all",
       let outputs = environment["TEST_UNDECLARED_OUTPUTS_DIR"],
       let root = snapshotRoot {
        directory = "\(outputs)/\(root)/\(fileName)"
        record = .all
    } else if let sourceDirectory = environment["TEST_SRCDIR"], let root = snapshotRoot {
        directory = "\(sourceDirectory)/_main/\(root)/\(fileName)"
        record = .never
    } else {
        directory = nil
        record = nil
    }

    let failure = verifySnapshot(
        of: try value(),
        as: snapshotting,
        named: name,
        record: record,
        snapshotDirectory: directory,
        file: filePath,
        testName: testName,
        line: line
    )
    if let failure {
        XCTFail(failure, file: filePath, line: line)
    }
}
