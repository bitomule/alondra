import Foundation
import SnapshotTesting
import XCTest

/// Bazel-aware wrapper around swift-snapshot-testing. Under Bazel `#filePath` never points at the
/// real source tree, so the snapshot directory comes from the environment:
///  - verify (plain `bazel test`): baselines are read from the runfiles at
///    `$TEST_SRCDIR/_main/<SNAPSHOT_ROOT>/<file>` and a missing baseline fails.
///  - record (`SNAPSHOT_TESTING_RECORD=failed`): each capture is first compared with its baseline
///    the way verify does, and only one that fails or has no baseline is written to
///    `$TEST_UNDECLARED_OUTPUTS_DIR/<SNAPSHOT_ROOT>/<file>`, which `make snapshots-record` copies back.
///    The renderer anti-aliases text and hairlines on glass one or two levels differently from one
///    test process to the next, so rewriting every baseline would change screens nobody touched.
///  - OS migration (`make snapshots-record-all`): all references go to a unique output generation;
///    the recorder copies them only when the whole reference set was captured.
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
       let outputs = environment["TEST_UNDECLARED_OUTPUTS_DIR"], let root = snapshotRoot,
       let generation = environment["SNAPSHOT_RECORD_GENERATION"] {
        directory = "\(outputs)/\(generation)/\(root)/\(fileName)"
        record = .all
    } else if environment["SNAPSHOT_TESTING_RECORD"] == "failed",
       let outputs = environment["TEST_UNDECLARED_OUTPUTS_DIR"],
       let sourceDirectory = environment["TEST_SRCDIR"],
       let root = snapshotRoot {
        let unchanged = verifySnapshot(
            of: try value(),
            as: snapshotting,
            named: name,
            record: .never,
            snapshotDirectory: "\(sourceDirectory)/_main/\(root)/\(fileName)",
            file: filePath,
            testName: testName,
            line: line
        ) == nil
        if unchanged { return }
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
