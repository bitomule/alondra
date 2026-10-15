public enum IrohCarrierError: Error, Sendable, Equatable {
    /// No live connection to the Mac's iroh endpoint; the request never left this device.
    case unavailable
    /// The Mac refused the request without running it.
    case refused(String)
    /// The read left whole but no answer came back.
    case noAnswer
}
