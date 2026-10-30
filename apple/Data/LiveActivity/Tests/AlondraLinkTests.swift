import AlondraLiveActivity
import Foundation
import Testing

@Suite struct AlondraLinkTests {
    @Test func eachLinkReadsBackFromItsURL() {
        for link in AlondraLink.allCases { #expect(AlondraLink(url: link.url) == link) }
        #expect(AlondraLink.inbox.url.absoluteString == "alondra://inbox")
        #expect(AlondraLink.tasks.url.absoluteString == "alondra://tasks")
    }

    @Test func anUnknownLinkOpensNothing() {
        for raw in ["alondra://settings", "alondra://inbox/card-1", "https://inbox", "alondra:inbox", "otherapp://tasks"] {
            #expect(AlondraLink(url: URL(string: raw)!) == nil, "\(raw)")
        }
    }
}
