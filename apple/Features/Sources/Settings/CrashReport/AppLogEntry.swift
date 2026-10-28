import Foundation

/// One line of the app's own log as `OSLogStore` gives it: the code's text around each placeholder
/// and the value that filled it, never the composed message.
public struct AppLogEntry: Equatable, Sendable {
    public struct Component: Equatable, Sendable {
        public enum Argument: Equatable, Sendable {
            case none
            case string(String)
            case number(String)
            case data
            case unknown
        }

        /// The code's text right before the placeholder (`formatSubstring`).
        public var text: String
        public var argument: Argument

        public init(text: String, argument: Argument = .none) {
            self.text = text; self.argument = argument
        }
    }

    public var date: Date
    public var category: String
    public var level: String
    public var components: [Component]

    public init(date: Date, category: String, level: String, components: [Component]) {
        self.date = date; self.category = category; self.level = level; self.components = components
    }
}
