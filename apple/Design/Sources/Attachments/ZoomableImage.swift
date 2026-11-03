import SwiftUI
#if canImport(UIKit)
import UIKit

/// A picture that fits the screen and zooms with a pinch or a double tap, and pans once zoomed.
struct ZoomableImage: UIViewRepresentable {
    let image: UIImage

    func makeUIView(context: Context) -> ZoomingScrollView { ZoomingScrollView(image: image) }

    func updateUIView(_ view: ZoomingScrollView, context: Context) { view.show(image) }
}

final class ZoomingScrollView: UIScrollView, UIScrollViewDelegate {
    private let imageView = UIImageView()

    init(image: UIImage) {
        super.init(frame: .zero)
        delegate = self
        minimumZoomScale = 1
        maximumZoomScale = 6
        showsVerticalScrollIndicator = false
        showsHorizontalScrollIndicator = false
        contentInsetAdjustmentBehavior = .never
        imageView.contentMode = .scaleAspectFit
        imageView.image = image
        addSubview(imageView)
        let doubleTap = UITapGestureRecognizer(target: self, action: #selector(doubleTapped(_:)))
        doubleTap.numberOfTapsRequired = 2
        addGestureRecognizer(doubleTap)
    }

    required init?(coder: NSCoder) { nil }

    func show(_ image: UIImage) {
        guard imageView.image !== image else { return }
        imageView.image = image
        setNeedsLayout()
    }

    override func layoutSubviews() {
        super.layoutSubviews()
        if zoomScale == 1, imageView.frame.size != bounds.size {
            imageView.frame = CGRect(origin: .zero, size: bounds.size)
            contentSize = bounds.size
        }
    }

    func viewForZooming(in scrollView: UIScrollView) -> UIView? { imageView }

    @objc private func doubleTapped(_ gesture: UITapGestureRecognizer) {
        if zoomScale > 1 {
            setZoomScale(1, animated: true)
        } else {
            let point = gesture.location(in: imageView)
            let size = CGSize(width: bounds.width / 3, height: bounds.height / 3)
            zoom(to: CGRect(x: point.x - size.width / 2, y: point.y - size.height / 2, width: size.width, height: size.height), animated: true)
        }
    }
}
#else
import AppKit

/// A picture that fits the window, zooms with a pinch on the trackpad or a double click, and is
/// dragged around once zoomed.
struct ZoomableImage: View {
    let image: NSImage
    @State private var scale: CGFloat = 1
    @State private var settledScale: CGFloat = 1
    @State private var offset: CGSize = .zero
    @State private var settledOffset: CGSize = .zero

    var body: some View {
        Image(nsImage: image).resizable().scaledToFit()
            .scaleEffect(scale)
            .offset(offset)
            .frame(maxWidth: .infinity, maxHeight: .infinity)
            .clipped()
            .contentShape(.rect)
            .gesture(MagnifyGesture()
                .onChanged { scale = min(max(settledScale * $0.magnification, 1), 8) }
                .onEnded { _ in
                    settledScale = scale
                    if scale == 1 { reset() }
                })
            .simultaneousGesture(DragGesture()
                .onChanged { drag in
                    guard scale > 1 else { return }
                    offset = CGSize(width: settledOffset.width + drag.translation.width,
                                    height: settledOffset.height + drag.translation.height)
                }
                .onEnded { _ in settledOffset = offset })
            .onTapGesture(count: 2) {
                withAnimation(.snappy) {
                    if scale > 1 {
                        reset()
                    } else {
                        scale = 2.5
                        settledScale = 2.5
                    }
                }
            }
    }

    private func reset() {
        scale = 1
        settledScale = 1
        offset = .zero
        settledOffset = .zero
    }
}
#endif
