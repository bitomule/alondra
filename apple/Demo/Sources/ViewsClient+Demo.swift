import AlondraFeed

public extension ViewsClient {
    /// Network-free documents passed through the production reducers and renderer.
    static func demo() -> ViewsClient { DemoWorkspace.client() }
}
