import Foundation

/// David's verdict on an artifact a node sent for review.
public enum ReviewVerdict: String, Sendable {
    /// The node reads David's comments on the artifact and applies them.
    case comments
    case approved
}
