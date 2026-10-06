import AlondraContract
import Foundation

public extension ConnectionProblem {
    var title: LocalizedStringResource {
        switch self {
        case .noInternet: "No internet connection"
        case .serverUnreachable: "Can’t reach the Alondra server"
        case .macNotAnswering: "Your Mac isn’t answering"
        case .alondraNotRunning: "Alondra isn’t running on this Mac"
        case .sessionExpired: "Your session has expired"
        case .deviceRevoked: "This device was disconnected from your Mac"
        case .macNotOnNetwork: "Your Mac isn’t on this network"
        case .macIdentityChanged: "This Mac’s identity has changed"
        case .signOutIncomplete: "Signing out didn’t finish"
        case .unexpected: "Can’t connect right now"
        }
    }

    var detail: LocalizedStringResource {
        switch self {
        case .noInternet: "This device is offline. Alondra reconnects as soon as the network is back."
        case .serverUnreachable: "You’re online, but the server isn’t answering. Alondra keeps trying."
        case .macNotAnswering: "It may be off, or Alondra may not be running on it. Alondra reconnects as soon as it answers."
        case .alondraNotRunning: "Run `alondra setup` in Terminal to start it. This window connects as soon as it answers."
        case .sessionExpired: "Sign in again to reconnect. Your pairing and saved work are kept."
        case .deviceRevoked: "Your Mac no longer accepts this device. Open Settings and link your Mac again."
        case .macNotOnNetwork: "Connect to the same Wi-Fi as your Mac, or turn on Tailscale on both. Alondra reconnects as soon as it answers."
        case .macIdentityChanged: "A Mac answered with this Mac’s name but different keys, so Alondra stopped. If you reinstalled Alondra on it, forget it in Settings and pair it again."
        case .signOutIncomplete: "Alondra couldn’t finish signing this device out. Finish signing out, then sign in again to reconnect."
        case .unexpected: "Something unexpected went wrong. Alondra keeps trying."
        }
    }

    var systemImage: String {
        switch self {
        case .noInternet: "wifi.slash"
        case .serverUnreachable: "icloud.slash"
        case .macNotAnswering: "desktopcomputer.trianglebadge.exclamationmark"
        case .alondraNotRunning: "bolt.horizontal.circle"
        case .sessionExpired: "person.crop.circle.badge.exclamationmark"
        case .deviceRevoked: "lock.slash"
        case .macNotOnNetwork: "wifi.exclamationmark"
        case .macIdentityChanged: "exclamationmark.shield"
        case .signOutIncomplete: "rectangle.portrait.and.arrow.right"
        case .unexpected: "exclamationmark.triangle"
        }
    }
}
