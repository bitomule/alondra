import Foundation

/// What the daemon on this Mac says about the cards waiting for the user: each card a phone was
/// told about, with that notice, and the whole queue. Read from `GET /notices` over loopback.
public struct LocalNotices: Decodable, Equatable, Sendable {
    public struct Notice: Decodable, Equatable, Sendable {
        public let id: Int64
        public let card: String
        public let title: String
        public let body: String?
        public let thread: String?
        public let level: String

        public init(id: Int64, card: String, title: String, body: String? = nil, thread: String? = nil, level: String = "active") {
            self.id = id; self.card = card; self.title = title; self.body = body; self.thread = thread; self.level = level
        }
    }

    public let dataEpoch: String
    public let notices: [Notice]
    public let open: [String]

    public init(dataEpoch: String, notices: [Notice], open: [String]) {
        self.dataEpoch = dataEpoch; self.notices = notices; self.open = open
    }
}
