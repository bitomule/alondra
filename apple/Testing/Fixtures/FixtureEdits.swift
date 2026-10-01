import AlondraContract
import Foundation
import XCTest

extension Fixtures {
    static func action(_ json: String) throws -> AlondraContract.Action {
        try ContractCodec.decode(AlondraContract.Action.self, from: Data(json.utf8))
    }

    static func response(_ json: String) throws -> ActionResponse {
        try ContractCodec.decode(ActionResponse.self, from: Data(json.utf8))
    }
}

extension Array where Element == [String: Any] {
    func sections(_ id: String) -> [String: Any]? { first { $0["id"] as? String == id } }
}

/// Replaces the items of one section of a fixture's JSON.
func replaceItems(of section: String, in object: inout [String: Any], with items: [[String: Any]]) {
    var sections = object["sections"] as? [[String: Any]] ?? []
    guard let index = sections.firstIndex(where: { $0["id"] as? String == section }) else { return }
    sections[index]["items"] = items
    object["sections"] = sections
}

func items(of section: String, in object: [String: Any]) -> [[String: Any]] {
    let sections = object["sections"] as? [[String: Any]] ?? []
    return sections.sections(section)?["items"] as? [[String: Any]] ?? []
}
