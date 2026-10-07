/// The path a Mac's connection uses right now, shown as a fact next to the Mac.
public enum ConnectionRoute: String, Equatable, Sendable, Codable {
    case thisMac
    case localNetwork
    case tailscale
    case relay
}
