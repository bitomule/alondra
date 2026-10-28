import Foundation
import Testing
@testable import AlondraTransport

@Suite struct MacSettingsRouteTests {
    private func request(_ method: HTTPMethod, _ path: [String], query: [(String, String)] = [], body: Data? = nil) throws -> APIRequest {
        try APIRequest(endpoint: Endpoint<Data>.data(method, path: path, query: query, body: body), operationID: "op-1")
    }

    @Test func theAppsReadAndChangeAMacsSettings() throws {
        #expect(try request(.get, ["settings"]).path == "/settings")
        #expect(try request(.put, ["settings"], body: Data("{}".utf8)).method == "PUT")
        #expect(try request(.get, ["settings", "folders"]).path == "/settings/folders")
        #expect(try request(.get, ["settings", "folders"], query: [("path", "/Users/d/My Code")]).path
            == "/settings/folders?path=%2FUsers%2Fd%2FMy%20Code")
    }

    @Test func anythingElseUnderSettingsIsRefused() {
        let refused: [(HTTPMethod, [String], [(String, String)])] = [
            (.get, ["settings"], [("path", "/Users/d")]),
            (.get, ["settings", "folders"], [("dir", "/Users/d")]),
            (.get, ["settings", "folders"], [("path", "/Users/d"), ("path", "/tmp")]),
            (.get, ["settings", "folders"], [("path", "")]),
            (.get, ["settings", "folders"], [("path", "/Users/d\n")]),
            (.get, ["settings", "other"], []),
            (.delete, ["settings"], []),
            (.put, ["settings", "folders"], []),
        ]
        for (method, path, query) in refused {
            #expect(throws: OperationError.invalidEndpoint, "\(method) \(path) \(query)") {
                try request(method, path, query: query, body: method == .get ? nil : Data("{}".utf8))
            }
        }
    }

    @Test func aPairedDeviceSealsTheSettingsRoutesForTheRelayAndTheLAN() {
        #expect(EnvelopeRequestPolicy.validViewsPath(method: "GET", path: "/settings", hasBody: false))
        #expect(EnvelopeRequestPolicy.validViewsPath(method: "PUT", path: "/settings", hasBody: true))
        #expect(EnvelopeRequestPolicy.validViewsPath(method: "GET", path: "/settings/folders?path=%2FUsers%2Fd", hasBody: false))
        #expect(!EnvelopeRequestPolicy.validViewsPath(method: "PUT", path: "/settings", hasBody: false))
        #expect(!EnvelopeRequestPolicy.validViewsPath(method: "PUT", path: "/actions", hasBody: true))
        #expect(!EnvelopeRequestPolicy.validViewsPath(method: "GET", path: "/settings/folders?dir=%2F", hasBody: false))
    }

    @Test func aDomainErrorKeepsItsMessageWhenItHasNoErrorKind() {
        let body = Data(#"{"code":"unavailable","message":"GitHub said no","current_revision":null,"subject":null}"#.utf8)
        #expect(APIError.from(status: 409, body: body) == .conflict(kind: nil, detail: "GitHub said no", reasons: []))
        let gone = Data(#"{"code":"not_found","message":"That report is no longer on this Mac"}"#.utf8)
        #expect(APIError.from(status: 404, body: gone)
            == .server(status: 404, kind: nil, detail: "That report is no longer on this Mac", reasons: []))
        #expect(APIError.from(status: 404, body: Data("{}".utf8)) == .unexpected(status: 404))
    }

    @Test func aRefusedFolderKeepsTheMacsTranslatedReason() {
        let body = Data(#"{"error":"not_a_directory","message":"~/notas es un archivo, no una carpeta."}"#.utf8)
        #expect(APIError.from(status: 422, body: body)
            == .server(status: 422, kind: "not_a_directory", detail: "~/notas es un archivo, no una carpeta.", reasons: []))
    }
}
