import AlondraContract
import Foundation
import XCTest

/// Views the tests draw from beyond the daemon's own fixtures: populated lists and full details.
enum Samples {
    static func document(_ name: String) throws -> ViewDocument {
        let environment = ProcessInfo.processInfo.environment
        let directory = URL(fileURLWithPath: try XCTUnwrap(environment["TEST_SRCDIR"]))
            .appendingPathComponent(environment["TEST_WORKSPACE"] ?? "_main")
            .appendingPathComponent("apple/Testing/Samples")
        return try ContractCodec.decode(ViewDocument.self, from: Data(contentsOf: directory.appendingPathComponent("\(name).json")))
    }

    /// A sample with one top-level field of one section replaced, as the daemon would send it changed.
    static func document(_ name: String, editing edit: (inout [String: Any]) -> Void) throws -> ViewDocument {
        let environment = ProcessInfo.processInfo.environment
        let url = URL(fileURLWithPath: try XCTUnwrap(environment["TEST_SRCDIR"]))
            .appendingPathComponent(environment["TEST_WORKSPACE"] ?? "_main")
            .appendingPathComponent("apple/Testing/Samples/\(name).json")
        var object = try XCTUnwrap(JSONSerialization.jsonObject(with: Data(contentsOf: url)) as? [String: Any])
        edit(&object)
        return try ContractCodec.decode(ViewDocument.self, from: JSONSerialization.data(withJSONObject: object))
    }
}
