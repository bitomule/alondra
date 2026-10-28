import AlondraFeatures
import Foundation
import Testing

/// The apps redact their logs by the CLI's rule: both run `fixtures/report/shape-vectors.json`.
@Suite struct ReportShapesTests {
    struct Vectors: Decodable {
        struct Split: Decodable { let value: String; let tokens: [String] }
        struct Value: Decodable { let value: String; let hint: String; let redacted: String }
        let vocabulary: [String]
        let safe: [String]
        let unsafe: [String]
        let split: [Split]
        let values: [Value]
    }

    static func vectors() throws -> Vectors {
        let environment = ProcessInfo.processInfo.environment
        let file = environment["TEST_SRCDIR"].map {
            URL(fileURLWithPath: $0).appendingPathComponent(environment["TEST_WORKSPACE"] ?? "_main")
                .appendingPathComponent("fixtures/report/shape-vectors.json")
        } ?? URL(fileURLWithPath: #filePath).deletingLastPathComponent()
            .appendingPathComponent("../../../../../fixtures/report/shape-vectors.json").standardizedFileURL
        return try JSONDecoder().decode(Vectors.self, from: Data(contentsOf: file))
    }

    @Test func theSharedVectorsHoldForTheApps() throws {
        let vectors = try Self.vectors()
        let vocabulary = Set(vectors.vocabulary)
        for token in vectors.safe {
            #expect(ReportShapes.isSafe(token, vocabulary: vocabulary), "\(token) should be safe")
        }
        for token in vectors.unsafe {
            #expect(!ReportShapes.isSafe(token, vocabulary: vocabulary), "\(token) should not be safe")
        }
        for split in vectors.split {
            #expect(ReportShapes.tokens(split.value) == split.tokens)
        }
        var redactor = ReportRedactor(vocabulary: vocabulary)
        for value in vectors.values {
            let hint = try #require(ReportShapes.Kind(rawValue: value.hint))
            #expect(redactor.value(value.value, hint: hint) == value.redacted, "\(value.value)")
        }
    }
}
