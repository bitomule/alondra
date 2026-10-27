import Foundation

/// The daemon requests a device may seal into an envelope, whichever carrier delivers it.
enum EnvelopeRequestPolicy {
    static func validClientContractPath(method: String, path: String) -> Bool {
        guard let method = HTTPMethod(rawValue: method), path.hasPrefix("/"),
              let components = URLComponents(string: "alondra://relay" + path) else { return false }
        let encoded = components.percentEncodedPath.split(separator: "/", omittingEmptySubsequences: false).dropFirst()
        guard let segments = encoded.map({ String($0).removingPercentEncoding }) as? [String],
              segments.allSatisfy({ !$0.isEmpty && $0 != "." && $0 != ".." }),
              APIRequest.validClientContractRoute(method: method, path: segments) else { return false }
        let query = components.queryItems?.compactMap { item -> (String, String)? in
            guard let value = item.value else { return nil }
            return (item.name, value)
        } ?? []
        guard query.count == (components.queryItems?.count ?? 0) else { return false }
        return APIRequest.validClientQuery(method: method, path: segments, query: query)
    }

    static func validViewsPath(method: String, path: String, hasBody: Bool) -> Bool {
        if method == "GET", !hasBody, validClientContractPath(method: method, path: path) { return true }
        if ["POST", "PUT"].contains(method), hasBody, validClientContractPath(method: method, path: path) { return true }
        guard method == "GET", !hasBody, path.hasPrefix("/"),
              let components = URLComponents(string: "alondra://relay" + path),
              components.fragment == nil else { return false }
        let encoded = components.percentEncodedPath.split(separator: "/", omittingEmptySubsequences: false)
        guard encoded.count == 4, encoded[0].isEmpty,
              let id = String(encoded[2]).removingPercentEncoding,
              APIRequest.validViewID(id) else { return false }
        if encoded[1] == "pull-requests", encoded[3] == "diff" {
            return components.queryItems == nil
        }
        if encoded[1] == "document-revisions", encoded[3] == "content",
           let query = components.queryItems {
            let names = query.map(\.name)
            guard Set(names).count == names.count, Set(names).isSubset(of: ["offset", "version"]),
                  let offset = query.first(where: { $0.name == "offset" })?.value.flatMap(UInt64.init),
                  offset <= 32 * 1024 * 1024 else { return false }
            return query.first(where: { $0.name == "version" })?.value.map { !$0.isEmpty && $0.utf8.count <= 128 } ?? true
        }
        return false
    }

    static func viewsChangeWait(_ path: String) -> Int? {
        guard path.hasPrefix("/changes?"), let components = URLComponents(string: path),
              let items = components.queryItems else { return nil }
        let allowed: Set<String> = ["protocol", "client", "lang", "wait", "cursor", "views", "capabilities"]
        let names = items.map(\.name)
        guard Set(names).isSubset(of: allowed), Set(names).count == names.count,
              ["protocol", "client", "lang", "wait"].allSatisfy(names.contains),
              items.first(where: { $0.name == "protocol" })?.value == "1",
              ["ios", "mac", "tui"].contains(items.first(where: { $0.name == "client" })?.value ?? ""),
              ["en", "es"].contains(items.first(where: { $0.name == "lang" })?.value ?? ""),
              let wait = items.first(where: { $0.name == "wait" })?.value.flatMap(Int.init), (0...50).contains(wait)
        else { return nil }
        if let capabilities = items.first(where: { $0.name == "capabilities" })?.value,
           !APIRequest.validViewCapabilities(capabilities) { return nil }
        return wait
    }
}
