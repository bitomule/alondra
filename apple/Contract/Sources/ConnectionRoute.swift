/// The path a Mac's connection uses right now, shown as a fact next to the Mac.
public enum ConnectionRoute: String, Equatable, Sendable, Codable {
    case thisMac
    case localNetwork
    case tailscale
    /// The relay's mailbox: the Mac picks each request up and leaves its answer.
    case relay
    /// iroh straight to the Mac, on the LAN or across the internet.
    case direct
    /// iroh through the relay, which streams each request to the Mac as it arrives.
    case irohRelay
}
