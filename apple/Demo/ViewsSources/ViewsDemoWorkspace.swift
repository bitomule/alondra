import AlondraViewsDependency

/// Network-free documents passed through the production Views reducer and renderer.
public enum ViewsDemoWorkspace {
    public static func client() -> ViewsClient { DemoViews.client() }
}
